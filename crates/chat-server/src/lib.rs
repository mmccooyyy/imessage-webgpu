//! Axum server (spec art_d3wTgg6t §3–§5): one process serves the client
//! bundle from `dist/`, upgrades `/ws`, and answers `/healthz`.
//!
//! Room state — joined users, a 200-message history ring, a monotonically
//! increasing `seq` — lives behind a `std::sync::Mutex`. Every mutation and
//! every seq-bearing broadcast happens while holding that lock, which is
//! what makes `welcome` + `resume` gap-free: a connection subscribes to the
//! broadcast bus before it takes the lock for its welcome, so no message can
//! be stamped and broadcast past it unseen. Lock sections are synchronous and
//! short — the guard is never held across an await.
//!
//! Each connection is one task split in two halves: a reader that applies
//! client frames to the room, and a pump that owns the sink half and
//! forwards broadcast frames, direct replies (welcome, acks, resume
//! replays), pings, and typing-expiry sweeps.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{State, WebSocketUpgrade},
    response::Response,
    routing::get,
    Router,
};
use chat_protocol::{ClientFrame, Msg, Peer, ServerFrame};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc};
use tower_http::services::ServeDir;
use uuid::Uuid;

/// Ring-buffer size for room history (spec §3: ~200 messages).
const HISTORY_CAP: usize = 200;
/// Server pings roughly every 25 s (spec §5.3). A ping left unanswered when
/// the next one falls due marks the peer gone; the socket is closed and
/// presence leave follows from the normal drop path.
const PING_INTERVAL: Duration = Duration::from_secs(25);
/// Typing deadlines lapse 3 s after the last refresh (spec §4). The room
/// sweeps them once a second and relays an explicit `typing{on:false}` —
/// belt and braces on top of the clients' own expiry.
const TYPING_TTL: Duration = Duration::from_secs(3);
const TYPING_TICK: Duration = Duration::from_secs(1);
/// A connected client must say hello within 10 s or the socket is dropped.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Broadcast buffer per connection. A pump that falls behind laps and drops
/// frames; the client resyncs via `resume` (clients render by server seq).
const BROADCAST_CAP: usize = 256;

/// Shared server state: the room behind a mutex plus the broadcast bus every
/// connection pump subscribes to.
#[derive(Clone)]
struct AppState {
    room: Arc<Mutex<Room>>,
    tx: broadcast::Sender<ServerFrame>,
}

/// Build the router. Separate from `main` so tests can drive it without
/// binding a fixed port.
pub fn router() -> Router {
    let state = AppState {
        room: Arc::new(Mutex::new(Room::new())),
        tx: broadcast::channel(BROADCAST_CAP).0,
    };
    Router::new()
        .route("/healthz", get(healthz))
        .route("/ws", get(ws_handler))
        .fallback_service(ServeDir::new("dist/"))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: axum::extract::ws::WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();
    let mut bcast_rx = state.tx.subscribe();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<axum::extract::ws::Message>();

    // Handshake (spec §3 step 1–2): the first frame must be `hello{name}` —
    // anything else gets `error{code:"no_hello"}` and a close.
    let joined: Option<Peer> = match tokio::time::timeout(HANDSHAKE_TIMEOUT, stream.next()).await {
        Ok(Some(Ok(axum::extract::ws::Message::Text(text)))) => {
            match serde_json::from_str::<ClientFrame>(&text) {
                Ok(ClientFrame::Hello { name }) => {
                    let mut room = state.room.lock().expect("room mutex poisoned");
                    let me = room.join(name);
                    let welcome = room.welcome_frame(&me.id);
                    // Announce the join under the lock so every connection's
                    // presence stream stays consistent with the room's state.
                    let _ = state.tx.send(ServerFrame::Presence {
                        join: Some(me.clone()),
                        leave: None,
                    });
                    drop(room);
                    // Queued before the pump starts, so welcome is the first
                    // thing this socket sends.
                    let _ = out_tx.send(text_frame(&welcome));
                    Some(me)
                }
                _ => {
                    let _ = sink.send(text_frame(&no_hello_error())).await;
                    // Complete the close handshake — a bare drop reads as a
                    // connection reset on the client side.
                    let _ = sink.send(axum::extract::ws::Message::Close(None)).await;
                    None
                }
            }
        }
        // Timeout, client close, or a non-text first frame: nothing to say.
        _ => None,
    };

    let Some(me) = joined else {
        return; // never joined — no presence leave to announce
    };

    // Outbound pump: owns the sink half. Forwards broadcast frames (skipping
    // this connection's own), direct replies from the reader, pings, and
    // typing-expiry sweeps.
    let pings_outstanding = Arc::new(AtomicU32::new(0));
    let pump = {
        let state = state.clone();
        let pump_me = me.id.clone();
        let pump_pings = Arc::clone(&pings_outstanding);
        tokio::spawn(async move {
            let mut ping_interval = tokio::time::interval(PING_INTERVAL);
            let mut expiry_tick = tokio::time::interval(TYPING_TICK);
            ping_interval.tick().await; // consume the immediate first tick
            expiry_tick.tick().await;
            loop {
                tokio::select! {
                    _ = ping_interval.tick() => {
                        let outstanding = pump_pings.fetch_add(1, Ordering::SeqCst) + 1;
                        if outstanding > 1 {
                            // The previous ping was never answered — the peer
                            // is gone; close and let the drop path clean up.
                            let _ = sink.send(axum::extract::ws::Message::Close(None)).await;
                            break;
                        }
                        if sink.send(axum::extract::ws::Message::Ping(axum::body::Bytes::new())).await.is_err() {
                            break;
                        }
                    }
                    _ = expiry_tick.tick() => {
                        let expired = {
                            let mut room = state.room.lock().expect("room mutex poisoned");
                            room.expire_typing(Instant::now())
                        };
                        for user in expired {
                            let _ = state.tx.send(ServerFrame::Typing { user, on: false });
                        }
                    }
                    reply = out_rx.recv() => match reply {
                        Some(msg) => {
                            if sink.send(msg).await.is_err() {
                                break;
                            }
                        }
                        // The reader side is gone; the connection is over.
                        None => break,
                    },
                    frame = bcast_rx.recv() => match frame {
                        Ok(frame) => {
                            if is_authored_by(&frame, &pump_me) {
                                continue;
                            }
                            if sink.send(text_frame(&frame)).await.is_err() {
                                break;
                            }
                        }
                        // Fell behind: drop frames; the client resyncs via
                        // `resume` (it renders by server seq).
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                }
            }
        })
    };

    // Reader loop: owns the stream half. Replies and room mutations flow
    // through `out_tx` and the broadcast bus — never through the sink here.
    let session = {
        let state = state.clone();
        let me = me.clone();
        let out_tx = out_tx.clone();
        let pings = Arc::clone(&pings_outstanding);
        async move {
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(axum::extract::ws::Message::Text(text)) => {
                        pings.store(0, Ordering::SeqCst);
                        match serde_json::from_str::<ClientFrame>(&text) {
                            Ok(frame) => {
                                handle_client_frame(frame, &me, &state, &out_tx);
                            }
                            Err(_) => {
                                let err = ServerFrame::Error {
                                    code: "bad_frame".into(),
                                    message: "unrecognized client frame".into(),
                                };
                                let _ = out_tx.send(text_frame(&err));
                            }
                        }
                    }
                    // Any liveness signal resets the missed-pong counter.
                    Ok(
                        axum::extract::ws::Message::Ping(_) | axum::extract::ws::Message::Pong(_),
                    ) => {
                        pings.store(0, Ordering::SeqCst);
                    }
                    Ok(axum::extract::ws::Message::Close(_)) => break,
                    Ok(_) => {} // binary frames are not part of the protocol
                    Err(_) => break,
                }
            }
        }
    };
    tokio::pin!(session);

    tokio::select! {
        // Client closed or errored; dropping the session future releases the
        // stream half, and the original `out_tx` drops with this frame so the
        // pump sees `None` and exits.
        _ = &mut session => {}
        // Pump gave up (peer unresponsive, sink failed): dropping the session
        // future closes the socket from this side.
        _ = pump => {}
    }

    // Presence leave on every exit path once joined (spec §3 step 4).
    let me_id = me.id;
    {
        let mut room = state.room.lock().expect("room mutex poisoned");
        room.leave(&me_id);
        let _ = state.tx.send(ServerFrame::Presence {
            join: None,
            leave: Some(me_id),
        });
    }
}

/// Apply one post-handshake client frame to the room (spec §3 step 3, §4).
/// Fully synchronous: replies ride `out_tx`, broadcasts ride the bus.
fn handle_client_frame(
    frame: ClientFrame,
    me: &Peer,
    state: &AppState,
    out_tx: &mpsc::UnboundedSender<axum::extract::ws::Message>,
) {
    match frame {
        // A second hello mid-session is a protocol error, not a rejoin.
        ClientFrame::Hello { .. } => {
            let err = ServerFrame::Error {
                code: "already_hello".into(),
                message: "hello may only be the first frame".into(),
            };
            let _ = out_tx.send(text_frame(&err));
        }
        ClientFrame::Msg { cmid, text } => {
            let ack = {
                let mut room = state.room.lock().expect("room mutex poisoned");
                match room.accept_msg(me.id.clone(), me.name.clone(), cmid, text, unix_now()) {
                    MsgOutcome::Recorded(msg) => {
                        // Broadcast under the lock so seq assignment and
                        // delivery stay atomic with respect to welcomes.
                        let _ = state.tx.send(ServerFrame::Msg {
                            id: msg.id.clone(),
                            from: msg.from.clone(),
                            name: msg.name.clone(),
                            text: msg.text.clone(),
                            ts: msg.ts,
                            seq: msg.seq,
                        });
                        ServerFrame::Ack {
                            cmid,
                            id: msg.id,
                            seq: msg.seq,
                        }
                    }
                    // Retried send: re-ack the original, change nothing.
                    MsgOutcome::Duplicate { id, seq } => ServerFrame::Ack { cmid, id, seq },
                }
            };
            let _ = out_tx.send(text_frame(&ack));
        }
        ClientFrame::Typing { on } => {
            {
                let mut room = state.room.lock().expect("room mutex poisoned");
                room.set_typing(&me.id, on, Instant::now());
            }
            // Relayed as-is; receivers expire it after ~3 s of silence.
            let _ = state.tx.send(ServerFrame::Typing {
                user: me.id.clone(),
                on,
            });
        }
        ClientFrame::Resume { last_seq } => {
            let replay = {
                let room = state.room.lock().expect("room mutex poisoned");
                room.replay_after(last_seq)
            };
            for msg in replay {
                let frame = ServerFrame::Msg {
                    id: msg.id,
                    from: msg.from,
                    name: msg.name,
                    text: msg.text,
                    ts: msg.ts,
                    seq: msg.seq,
                };
                if out_tx.send(text_frame(&frame)).is_err() {
                    return;
                }
            }
        }
    }
}

fn no_hello_error() -> ServerFrame {
    ServerFrame::Error {
        code: "no_hello".into(),
        message: "first frame must be hello".into(),
    }
}

/// True when a broadcast frame was authored by the given user, so the pump
/// can skip self-echoes: senders get `ack` instead of their own `msg`.
fn is_authored_by(frame: &ServerFrame, id: &str) -> bool {
    match frame {
        ServerFrame::Msg { from, .. } => from == id,
        ServerFrame::Typing { user, .. } => user == id,
        ServerFrame::Presence { join, leave } => {
            join.as_ref().is_some_and(|p| p.id == id) || leave.as_deref() == Some(id)
        }
        _ => false,
    }
}

fn text_frame(frame: &ServerFrame) -> axum::extract::ws::Message {
    axum::extract::ws::Message::Text(
        serde_json::to_string(frame)
            .expect("server frames serialize")
            .into(),
    )
}

fn unix_now() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is sane")
        .as_secs();
    i64::try_from(secs).unwrap_or(i64::MAX)
}

/// Result of handing a `msg` frame to the room: either it was recorded (and
/// must be broadcast), or the `cmid` was already seen — the sender just needs
/// a repeat ack, with no new seq and no broadcast (spec §4 dedupe rule).
enum MsgOutcome {
    Recorded(Msg),
    Duplicate { id: String, seq: u64 },
}

/// In-memory room state (spec §3): joined users, the history ring, the seq
/// cursor, seen `cmid`s for idempotent retries, and typing deadlines.
/// Guarded by `AppState.room`'s mutex; never touched across an await.
struct Room {
    users: BTreeMap<String, Peer>,
    history: VecDeque<Msg>,
    /// `cmid` → (message id, seq) of already-accepted messages. Unbounded:
    /// one entry per accepted send, so demo-scale by construction.
    seen: HashMap<Uuid, (String, u64)>,
    typing: HashMap<String, Instant>,
    next_seq: u64,
    next_user: u64,
}

impl Room {
    fn new() -> Self {
        Self {
            users: BTreeMap::new(),
            history: VecDeque::new(),
            seen: HashMap::new(),
            typing: HashMap::new(),
            next_seq: 0,
            next_user: 0,
        }
    }

    fn join(&mut self, name: String) -> Peer {
        self.next_user += 1;
        let peer = Peer {
            id: format!("u_{}", self.next_user),
            name,
        };
        self.users.insert(peer.id.clone(), peer.clone());
        peer
    }

    fn leave(&mut self, id: &str) {
        self.users.remove(id);
        self.typing.remove(id);
    }

    /// The welcome snapshot for `you`: peers other than yourself, the full
    /// history ring oldest-first, and the seq cursor to resume from.
    fn welcome_frame(&self, you: &str) -> ServerFrame {
        ServerFrame::Welcome {
            you: you.to_string(),
            users: self
                .users
                .values()
                .filter(|p| p.id != you)
                .cloned()
                .collect(),
            history: self.history.iter().cloned().collect(),
            seq: self.next_seq,
        }
    }

    fn accept_msg(
        &mut self,
        from: String,
        name: String,
        cmid: Uuid,
        text: String,
        ts: i64,
    ) -> MsgOutcome {
        if let Some((id, seq)) = self.seen.get(&cmid) {
            return MsgOutcome::Duplicate {
                id: id.clone(),
                seq: *seq,
            };
        }
        // 1-based: `next_seq` is the last assigned seq — the same cursor the
        // client passes back as `resume{last_seq}` — so nothing (including
        // the very first message) can fall below a fresh client's cursor of 0.
        let seq = self.next_seq + 1;
        self.next_seq = seq;
        let msg = Msg {
            id: format!("m_{seq}"),
            from,
            name,
            text,
            ts,
            seq,
        };
        self.seen.insert(cmid, (msg.id.clone(), seq));
        self.history.push_back(msg.clone());
        while self.history.len() > HISTORY_CAP {
            self.history.pop_front();
        }
        MsgOutcome::Recorded(msg)
    }

    /// Every history message with `seq > last_seq`, oldest-first.
    fn replay_after(&self, last_seq: u64) -> Vec<Msg> {
        self.history
            .iter()
            .filter(|m| m.seq > last_seq)
            .cloned()
            .collect()
    }

    fn set_typing(&mut self, id: &str, on: bool, now: Instant) {
        if on {
            self.typing.insert(id.to_string(), now + TYPING_TTL);
        } else {
            self.typing.remove(id);
        }
    }

    /// Drop lapsed typing deadlines, returning the users whose indicators
    /// just died (callers relay `typing{on:false}` for each).
    fn expire_typing(&mut self, now: Instant) -> Vec<String> {
        let expired: Vec<String> = self
            .typing
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            self.typing.remove(id);
        }
        expired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room_with_users(n: u64) -> Room {
        let mut room = Room::new();
        for i in 1..=n {
            room.join(format!("user {i}"));
        }
        room
    }

    fn accept(room: &mut Room, cmid: Uuid, text: &str) -> Msg {
        match room.accept_msg("u_1".into(), "user 1".into(), cmid, text.into(), 0) {
            MsgOutcome::Recorded(msg) => msg,
            MsgOutcome::Duplicate { .. } => panic!("unexpected duplicate"),
        }
    }

    #[test]
    fn join_assigns_sequential_ids() {
        let mut room = Room::new();
        assert_eq!(room.join("Matt".into()).id, "u_1");
        assert_eq!(room.join("Sam".into()).id, "u_2");
        room.leave("u_1");
        // Ids never repeat, even after a leave.
        assert_eq!(room.join("Pat".into()).id, "u_3");
        assert_eq!(room.users.len(), 2);
    }

    #[test]
    fn welcome_excludes_self_and_lists_others_in_order() {
        let mut room = room_with_users(2);
        let msg = accept(&mut room, Uuid::new_v4(), "hi");
        let welcome = room.welcome_frame("u_2");
        let ServerFrame::Welcome {
            users,
            history,
            seq,
            ..
        } = welcome
        else {
            panic!("expected welcome");
        };
        assert_eq!(
            users,
            vec![Peer {
                id: "u_1".into(),
                name: "user 1".into()
            }]
        );
        assert_eq!(history, vec![msg]);
        assert_eq!(seq, 1);
    }

    #[test]
    fn msg_assigns_monotonic_seq_from_one() {
        let mut room = Room::new();
        assert_eq!(accept(&mut room, Uuid::new_v4(), "a").seq, 1);
        assert_eq!(accept(&mut room, Uuid::new_v4(), "b").seq, 2);
        assert_eq!(accept(&mut room, Uuid::new_v4(), "c").seq, 3);
    }

    #[test]
    fn duplicate_cmid_reacks_without_recording() {
        let mut room = Room::new();
        room.join("Matt".into());
        let cmid = Uuid::new_v4();
        let first = accept(&mut room, cmid, "once");

        match room.accept_msg("u_1".into(), "Matt".into(), cmid, "once".into(), 5) {
            MsgOutcome::Duplicate { id, seq } => {
                assert_eq!((id.as_str(), seq), (first.id.as_str(), first.seq));
            }
            MsgOutcome::Recorded(_) => panic!("retry must not re-record"),
        }
        // No new seq, no history growth.
        assert_eq!(room.next_seq, 1);
        assert_eq!(room.history.len(), 1);
    }

    #[test]
    fn history_cap_evicts_oldest() {
        let mut room = Room::new();
        room.join("Matt".into());
        for i in 1..=HISTORY_CAP + 10 {
            accept(&mut room, Uuid::new_v4(), &format!("message {i}"));
        }
        assert_eq!(room.history.len(), HISTORY_CAP);
        assert_eq!(room.history.front().unwrap().seq, 11, "oldest evicted");
        assert_eq!(room.history.back().unwrap().seq, HISTORY_CAP as u64 + 10);
    }

    #[test]
    fn replay_after_returns_exactly_the_later_range() {
        let mut room = Room::new();
        room.join("Matt".into());
        for i in 1..=3 {
            accept(&mut room, Uuid::new_v4(), &format!("m{i}"));
        }
        let seqs =
            |last: u64| -> Vec<u64> { room.replay_after(last).iter().map(|m| m.seq).collect() };
        assert_eq!(room.replay_after(0).len(), 3);
        assert_eq!(seqs(1), vec![2, 3]);
        assert!(seqs(3).is_empty());
        assert!(seqs(99).is_empty());
    }

    #[test]
    fn typing_expires_after_ttl() {
        let mut room = Room::new();
        room.join("Matt".into());
        let t0 = Instant::now();
        room.set_typing("u_1", true, t0);

        assert!(room
            .expire_typing(t0 + Duration::from_millis(2500))
            .is_empty());
        assert_eq!(room.expire_typing(t0 + TYPING_TTL), vec!["u_1".to_string()]);
        // Expired entries do not fire twice.
        assert!(room.expire_typing(t0 + TYPING_TTL + TYPING_TICK).is_empty());
    }

    #[test]
    fn typing_off_clears_deadline() {
        let mut room = Room::new();
        room.join("Matt".into());
        let t0 = Instant::now();
        room.set_typing("u_1", true, t0);
        room.set_typing("u_1", false, t0);
        assert!(room.expire_typing(t0 + TYPING_TTL * 2).is_empty());
    }

    #[test]
    fn leave_removes_user_and_typing() {
        let mut room = room_with_users(1);
        room.set_typing("u_1", true, Instant::now());
        room.leave("u_1");
        assert!(room.users.is_empty());
        assert!(room
            .expire_typing(Instant::now() + TYPING_TTL * 2)
            .is_empty());
    }
}
