//! Wire protocol for the chat: JSON text frames tagged by `"t"`, exchanged
//! over the server's `/ws` WebSocket (spec art_d3wTgg6t §4).
//!
//! Invariants this crate owns:
//! - the server assigns a total order (`seq`); clients never order locally
//! - the client generates a `cmid` per send; retries are idempotent
//! - unknown server tags deserialize into a tolerant variant instead of
//!   failing the stream
//!
//! No wasm and no server-only dependencies — this crate compiles into both
//! the browser bundle and the axum host, natively tested on each PR.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A participant as the server sees it: server-assigned id plus chosen name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Peer {
    pub id: String,
    pub name: String,
}

/// A chat message in server-assigned order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Msg {
    pub id: String,
    pub from: String,
    pub name: String,
    pub text: String,
    /// Unix epoch seconds.
    pub ts: i64,
    pub seq: u64,
}

/// Client → server frames.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientFrame {
    /// First frame after the socket opens; the server assigns the user id and
    /// seq cursor. Anything else gets `Error{code:"no_hello"}` and a close.
    Hello { name: String },
    /// A chat message. Retries on reconnect reuse the same `cmid`; the server
    /// dedupes on it, so redelivery never renders twice.
    Msg { cmid: Uuid, text: String },
    /// Typing state; receivers auto-expire after ~3 s without a refresh.
    Typing { on: bool },
    /// After a reconnect: replay everything after `last_seq`, in `seq` order.
    Resume { last_seq: u64 },
}

/// Server → client frames.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerFrame {
    /// Reply to `hello`: your identity, current peers, ring-buffer history,
    /// and the seq cursor to resume from.
    Welcome {
        you: String,
        users: Vec<Peer>,
        history: Vec<Msg>,
        seq: u64,
    },
    /// A chat message broadcast to everyone except its sender, who instead
    /// gets `ack` against the original `cmid`.
    Msg {
        id: String,
        from: String,
        name: String,
        text: String,
        ts: i64,
        seq: u64,
    },
    /// Sender-only delivery confirmation; flips the pending bubble to
    /// "Delivered" and pins the message's place in the total order.
    Ack { cmid: Uuid, id: String, seq: u64 },
    /// A peer's typing state, relayed as-is; expiry is client-side.
    Typing { user: String, on: bool },
    /// Presence change: someone joined (peer info) or left (their user id).
    Presence {
        join: Option<Peer>,
        leave: Option<String>,
    },
    /// Protocol-level error (e.g. `no_hello` for a wrong first frame).
    Error { code: String, message: String },
}

/// Tolerant envelope over server frames: a frame whose `"t"` tag this build
/// does not know lands in [`AnyFrame::Unknown`] instead of failing the
/// stream — forward compatibility across a mixed-version demo.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AnyFrame {
    Known(ServerFrame),
    Unknown(serde_json::Value),
}
