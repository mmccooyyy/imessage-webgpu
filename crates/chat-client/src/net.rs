//! WebSocket transport (spec art_d3wTgg6t §5): one gloo-net websocket per
//! connection, split into a reader task and a writer task.
//!
//! - The reader pumps server text frames through the tolerant
//!   `AnyFrame` parser and forwards [`NetEvent`]s to the app over a std
//!   channel (drained each rAF frame — wasm is single-threaded).
//! - The writer owns the sink side and drains an unbounded channel, so the
//!   rAF loop can enqueue frames synchronously.
//! - Frames queued while no socket is live live in the app's outbox and are
//!   flushed (hello first) into the next connection's writer channel.
//!
//! wasm-only: gloo-net's websocket surface does not exist on the host.

use std::collections::VecDeque;
use std::sync::mpsc::Sender;

use chat_protocol::{AnyFrame, ClientFrame};
use futures::{SinkExt, StreamExt};
use gloo_net::websocket::{futures::WebSocket, Message};

/// Events the socket tasks hand back to the app loop.
#[derive(Debug)]
pub enum NetEvent {
    /// A decoded, known server frame. Unknown tags were absorbed upstream.
    Frame(chat_protocol::ServerFrame),
    /// The connection ended (open failure, stream error, or close). The
    /// store owns the reconnect policy.
    Closed,
}

/// Open a websocket and spawn its reader/writer tasks. `initial` is flushed
/// into the writer channel before anything else — `hello` MUST be the first
/// frame on a fresh connection (the server rejects anything else).
///
/// `None` when the socket could not even be created; the store treats that
/// like any dropped connection (reconnect/backoff is its policy).
pub fn connect(
    url: &str,
    initial: VecDeque<ClientFrame>,
    events: Sender<NetEvent>,
) -> Option<futures::channel::mpsc::UnboundedSender<ClientFrame>> {
    let (outbound_tx, mut outbound_rx) = futures::channel::mpsc::unbounded::<ClientFrame>();

    let ws = match WebSocket::open(url) {
        Ok(ws) => ws,
        Err(e) => {
            log::warn!("websocket open failed: {e}");
            // The store treats this like any dropped connection.
            let _ = events.send(NetEvent::Closed);
            return None;
        }
    };
    let (mut sink, stream) = ws.split();

    for frame in initial {
        let _ = outbound_tx.unbounded_send(frame);
    }

    // Writer: forward queued frames; a send failure ends the task and the
    // reader's close event handles the state transition.
    wasm_bindgen_futures::spawn_local(async move {
        while let Some(frame) = outbound_rx.next().await {
            match serde_json::to_string(&frame) {
                Ok(json) => {
                    if sink.send(Message::Text(json)).await.is_err() {
                        log::warn!("websocket send failed — writer exiting");
                        break;
                    }
                }
                Err(e) => {
                    // A frame that cannot serialize is a protocol bug — log
                    // loudly rather than dropping it silently.
                    log::error!("frame serialization failed: {e}");
                }
            }
        }
    });

    // Reader: pump decoded frames back to the app.
    spawn_local_reader(stream, events);
    Some(outbound_tx)
}

fn spawn_local_reader(
    mut stream: futures::stream::SplitStream<WebSocket>,
    events: Sender<NetEvent>,
) {
    wasm_bindgen_futures::spawn_local(async move {
        loop {
            match stream.next().await {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<AnyFrame>(&text) {
                    Ok(AnyFrame::Known(frame)) => {
                        if events.send(NetEvent::Frame(frame)).is_err() {
                            break; // app gone — nothing to serve
                        }
                    }
                    Ok(AnyFrame::Unknown(value)) => {
                        // Tolerant variant absorbed it: log, never fail the stream.
                        log::info!("ignoring unknown server frame: {value}");
                    }
                    Err(e) => log::error!("frame parse failed: {e} — {text}"),
                },
                Some(Ok(Message::Bytes(_))) => {} // the protocol is text-only
                Some(Err(e)) => {
                    log::warn!("websocket error: {e}");
                    break;
                }
                None => break, // connection closed
            }
        }
        let _ = events.send(NetEvent::Closed);
    });
}
