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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CMID: &str = "67e55044-22b1-4f2f-9a0b-12f45a1c7f3d";

    fn cmid() -> Uuid {
        Uuid::parse_str(CMID).expect("test cmid")
    }

    fn sample_msg() -> Msg {
        Msg {
            id: "m_5".into(),
            from: "u_2".into(),
            name: "Sam".into(),
            text: "hi".into(),
            ts: 1_760_000_000,
            seq: 413,
        }
    }

    fn sample_server_msg() -> ServerFrame {
        let m = sample_msg();
        ServerFrame::Msg {
            id: m.id,
            from: m.from,
            name: m.name,
            text: m.text,
            ts: m.ts,
            seq: m.seq,
        }
    }

    /// Serialize → deserialize → compare, and assert the `"t"` tag survived.
    fn round_trip<T>(value: T, tag: &str)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(&value).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["t"], tag, "tag key must be \"t\": {json}");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, value);
    }

    #[test]
    fn client_frames_round_trip() {
        round_trip(
            ClientFrame::Hello {
                name: "Matt".into(),
            },
            "hello",
        );
        round_trip(
            ClientFrame::Msg {
                cmid: cmid(),
                text: "hi".into(),
            },
            "msg",
        );
        round_trip(ClientFrame::Typing { on: true }, "typing");
        round_trip(ClientFrame::Resume { last_seq: 412 }, "resume");
    }

    #[test]
    fn server_frames_round_trip() {
        let peer = Peer {
            id: "u_2".into(),
            name: "Sam".into(),
        };
        round_trip(
            ServerFrame::Welcome {
                you: "u_1".into(),
                users: vec![peer.clone()],
                history: vec![sample_msg()],
                seq: 412,
            },
            "welcome",
        );
        round_trip(sample_server_msg(), "msg");
        round_trip(
            ServerFrame::Ack {
                cmid: cmid(),
                id: "m_5".into(),
                seq: 413,
            },
            "ack",
        );
        round_trip(
            ServerFrame::Typing {
                user: "u_2".into(),
                on: true,
            },
            "typing",
        );
        round_trip(
            ServerFrame::Presence {
                join: Some(peer.clone()),
                leave: None,
            },
            "presence",
        );
        round_trip(
            ServerFrame::Presence {
                join: None,
                leave: Some("u_2".into()),
            },
            "presence",
        );
        round_trip(
            ServerFrame::Error {
                code: "no_hello".into(),
                message: "first frame must be hello".into(),
            },
            "error",
        );
    }

    #[test]
    fn unknown_tags_land_in_the_tolerant_variant() {
        let frame: AnyFrame =
            serde_json::from_str(r#"{"t":"hologram","power":9001}"#).expect("tolerant parse");
        assert!(matches!(frame, AnyFrame::Unknown(_)));

        // Known frames still come through the envelope as Known.
        let frame: AnyFrame =
            serde_json::from_str(r#"{"t":"typing","user":"u_2","on":false}"#).expect("known");
        assert_eq!(
            frame,
            AnyFrame::Known(ServerFrame::Typing {
                user: "u_2".into(),
                on: false
            })
        );

        // A frame that fails its known shape (bad uuid) also degrades, not errors.
        let frame: AnyFrame =
            serde_json::from_str(r#"{"t":"ack","cmid":"not-a-uuid","id":"m_5","seq":9}"#)
                .expect("tolerant parse");
        assert!(matches!(frame, AnyFrame::Unknown(_)));
    }

    #[test]
    fn unknown_payload_is_preserved_verbatim() {
        let frame: AnyFrame = serde_json::from_str(r#"{"t":"hologram","power":9001}"#).unwrap();
        match frame {
            AnyFrame::Unknown(value) => {
                assert_eq!(value, json!({"t": "hologram", "power": 9001}));
            }
            AnyFrame::Known(_) => panic!("unknown tag must not parse as a known frame"),
        }
    }

    #[test]
    fn strict_server_frame_rejects_unknown_tags() {
        let result = serde_json::from_str::<ServerFrame>(r#"{"t":"hologram"}"#);
        assert!(
            result.is_err(),
            "tolerance is AnyFrame's job, not ServerFrame's"
        );
    }

    #[test]
    fn cmid_survives_byte_identical() {
        let frame = ClientFrame::Msg {
            cmid: cmid(),
            text: "hi".into(),
        };
        let first = serde_json::to_string(&frame).unwrap();
        assert!(first.contains(CMID));

        let back: ClientFrame = serde_json::from_str(&first).unwrap();
        let second = serde_json::to_string(&back).unwrap();
        assert_eq!(first, second, "re-serialization must be byte-identical");

        match back {
            ClientFrame::Msg { cmid, .. } => assert_eq!(cmid.to_string(), CMID),
            other => panic!("wrong variant: {other:?}"),
        }

        // The server's ack echoes the cmid unchanged too.
        let ack = ServerFrame::Ack {
            cmid: cmid(),
            id: "m_5".into(),
            seq: 413,
        };
        let ack_json = serde_json::to_string(&ack).unwrap();
        let ack_back: ServerFrame = serde_json::from_str(&ack_json).unwrap();
        match ack_back {
            ServerFrame::Ack { cmid, .. } => assert_eq!(cmid.to_string(), CMID),
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
