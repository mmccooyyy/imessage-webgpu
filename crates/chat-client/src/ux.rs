//! Pure shell helpers (spec art_d3wTgg6t §5): websocket URL derivation,
//! typing throttle math, and the store → scene mapping. Compiled on the host
//! too so their tests run with plain `cargo test` — no web-sys, no wgpu.

use chat_core::{ConnState, Store};
use chat_protocol::Peer;
use uuid::Uuid;

use crate::scene::{Banner, Message, Scene, Side};

/// Derive the `/ws` URL from the page's location (spec §5.4: never hard-code
/// the scheme or host — https→wss, http→ws, same origin as the page).
pub fn ws_url(protocol: &str, host: &str) -> String {
    let scheme = if protocol.eq_ignore_ascii_case("https:") {
        "wss"
    } else {
        "ws"
    };
    format!("{scheme}://{host}/ws")
}

/// Typing throttle: send `Typing{on:true}` at most once every
/// [`TYPING_TX_THROTTLE_MS`] while the user keeps typing (spec: ~2.5 s).
pub const TYPING_TX_THROTTLE_MS: u64 = 2_500;
/// Idle gap after the last keystroke before the shell sends `Typing{on:false}`.
pub const TYPING_OFF_IDLE_MS: u64 = 2_000;

/// Whether a `Typing{on:true}` frame is due at `now_ms`.
pub fn typing_on_due(last_sent_ms: Option<u64>, now_ms: u64) -> bool {
    match last_sent_ms {
        None => true,
        Some(last) => now_ms.saturating_sub(last) >= TYPING_TX_THROTTLE_MS,
    }
}

/// Whether the local typing indicator should be retired at `now_ms`.
pub fn typing_off_due(active: bool, last_keystroke_ms: u64, now_ms: u64) -> bool {
    active && now_ms.saturating_sub(last_keystroke_ms) >= TYPING_OFF_IDLE_MS
}

/// Presentation-only log of join/leave pills for the transcript ("Sam
/// joined" / "Sam left", spec §5). This is *not* conversation state — the
/// store owns who is currently present; this buffer only remembers what the
/// transcript has already shown, capped so a long demo cannot grow it.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PresenceLog {
    events: Vec<String>,
}

/// Pills kept in the transcript before older ones fall off the top.
const PRESENCE_LOG_CAP: usize = 8;

impl PresenceLog {
    pub fn push(&mut self, event: String) {
        self.events.push(event);
        if self.events.len() > PRESENCE_LOG_CAP {
            self.events.remove(0);
        }
    }

    pub fn items(&self) -> &[String] {
        &self.events
    }
}

/// Map one [`chat_core::ChatMessage`] into a renderer message.
fn scene_message(id: &str, text: &str, mine: bool, delivered: bool) -> Message {
    Message {
        id: id.to_string(),
        side: if mine { Side::Outgoing } else { Side::Incoming },
        text: text.to_string(),
        delivered,
    }
}

/// Resolve a peer id to its display name, falling back to the raw id.
pub fn peer_name(peers: &[Peer], id: &str) -> String {
    peers
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| id.to_string())
}

/// Build the frame's [`Scene`] from the store plus the presentation-only
/// presence log. `date_label` is the conversation-start pill text (shown
/// once there is anything to show); `now` is the store clock in millis.
pub fn build_scene(
    store: &Store,
    presence: &PresenceLog,
    date_label: Option<&str>,
    now: u64,
) -> Scene {
    let mut messages: Vec<Message> = store
        .messages()
        .iter()
        .map(|m| scene_message(&m.id, &m.text, m.mine, m.mine))
        .collect();
    // Pending sends trail the transcript as un-delivered outgoing bubbles.
    messages.extend(
        store
            .pending()
            .iter()
            .map(|p| scene_message(&p.cmid.to_string(), &p.text, true, false)),
    );

    let banner = match store.conn() {
        ConnState::Connecting => Some(Banner::Connecting),
        ConnState::Reconnecting { .. } => Some(Banner::Reconnecting),
        _ => None,
    };

    let typing_peer = store
        .typing_users(now)
        .first()
        .map(|id| peer_name(&store.peers().cloned().collect::<Vec<Peer>>(), id));

    let date_pill = if messages.is_empty() {
        None
    } else {
        date_label.map(str::to_string)
    };

    Scene {
        date_pill,
        messages,
        typing_peer,
        banner,
        pills: presence.items().to_vec(),
    }
}

/// The new `cmid` for a send. Wasm gets entropy through uuid's `js` feature.
pub fn new_cmid() -> Uuid {
    Uuid::new_v4()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chat_core::Event;
    use chat_protocol::{ClientFrame, ServerFrame};

    const CMID: Uuid = Uuid::nil();

    /// Drive a store through real events to a live, empty conversation.
    fn live_store() -> Store {
        let mut store = Store::new();
        let now = 1_000;
        store.apply(Event::SocketOpened, now);
        store.apply(
            Event::Frame(ServerFrame::Welcome {
                you: "u_1".into(),
                users: vec![
                    Peer {
                        id: "u_1".into(),
                        name: "Matt".into(),
                    },
                    Peer {
                        id: "u_2".into(),
                        name: "Sam".into(),
                    },
                ],
                history: vec![],
                seq: 0,
            }),
            now,
        );
        store
    }

    fn peer_msg(seq: u64, text: &str) -> ServerFrame {
        ServerFrame::Msg {
            id: format!("m_{seq}"),
            from: "u_2".into(),
            name: "Sam".into(),
            text: text.into(),
            ts: 0,
            seq,
        }
    }

    #[test]
    fn ws_url_follows_the_page_scheme() {
        assert_eq!(
            ws_url("https:", "chat.example.com"),
            "wss://chat.example.com/ws"
        );
        assert_eq!(ws_url("http:", "localhost:8080"), "ws://localhost:8080/ws");
        assert_eq!(ws_url("HTTPS:", "h"), "wss://h/ws");
        assert_eq!(ws_url("file:", "h"), "ws://h/ws");
    }

    #[test]
    fn typing_throttle_bounds() {
        // First keystroke always announces.
        assert!(typing_on_due(None, 0));
        assert!(typing_on_due(None, 10_000));
        // Refreshes inside the window are suppressed…
        assert!(!typing_on_due(
            Some(1_000),
            1_000 + TYPING_TX_THROTTLE_MS - 1
        ));
        // …and due again after it.
        assert!(typing_on_due(Some(1_000), 1_000 + TYPING_TX_THROTTLE_MS));
        // Idle retirement.
        assert!(!typing_off_due(false, 0, 10_000));
        assert!(!typing_off_due(true, 5_000, 5_000 + TYPING_OFF_IDLE_MS - 1));
        assert!(typing_off_due(true, 5_000, 5_000 + TYPING_OFF_IDLE_MS));
    }

    #[test]
    fn presence_log_caps_itself() {
        let mut log = PresenceLog::default();
        for i in 0..12 {
            log.push(format!("peer {i} joined"));
        }
        assert_eq!(log.items().len(), PRESENCE_LOG_CAP);
        assert_eq!(log.items().last().unwrap(), "peer 11 joined");
        assert_eq!(log.items().first().unwrap(), "peer 4 joined");
    }

    #[test]
    fn scene_maps_messages_pending_and_typing() {
        let mut store = live_store();
        let now = 5_000;
        // An incoming message at seq 1, then a pending send of our own.
        store.apply(Event::Frame(peer_msg(1, "incoming")), now);
        let _ = store.send("outgoing!".into(), CMID, now);
        store.apply(
            Event::Frame(ServerFrame::Typing {
                user: "u_2".into(),
                on: true,
            }),
            now,
        );

        let mut presence = PresenceLog::default();
        presence.push("Sam joined".into());
        let scene = build_scene(&store, &presence, Some("Today"), now);

        // Delivered messages in seq order, the pending bubble trailing.
        assert_eq!(scene.messages.len(), 2);
        assert_eq!(scene.messages[0].side, Side::Incoming);
        assert_eq!(scene.messages[0].text, "incoming");
        assert!(!scene.messages[0].delivered);
        assert_eq!(scene.messages[1].side, Side::Outgoing);
        assert_eq!(scene.messages[1].text, "outgoing!");
        assert!(!scene.messages[1].delivered); // pending → undelivered
        assert_eq!(scene.messages[1].id, CMID.to_string());
        assert_eq!(scene.typing_peer.as_deref(), Some("Sam"));
        assert_eq!(scene.date_pill.as_deref(), Some("Today"));
        assert_eq!(scene.banner, None);
        assert_eq!(scene.pills, vec!["Sam joined".to_string()]);
    }

    #[test]
    fn scene_reflects_connection_states() {
        // A fresh store starts Connecting.
        let store = Store::new();
        let scene = build_scene(&store, &PresenceLog::default(), None, 0);
        assert_eq!(scene.banner, Some(Banner::Connecting));

        // Closing a live session arms the reconnecting state.
        let mut store = live_store();
        let _ = store.apply(Event::SocketClosed { jitter: 0.5 }, 2_000);
        let scene = build_scene(&store, &PresenceLog::default(), None, 0);
        assert_eq!(scene.banner, Some(Banner::Reconnecting));

        // Reconnected and caught up: banner cleared.
        store.apply(Event::SocketOpened, 3_000);
        store.apply(
            Event::Frame(ServerFrame::Welcome {
                you: "u_1".into(),
                users: vec![],
                history: vec![],
                seq: 0,
            }),
            3_000,
        );
        let scene = build_scene(&store, &PresenceLog::default(), None, 0);
        assert_eq!(scene.banner, None);

        // Empty conversation: no date pill even when a label is offered.
        assert_eq!(scene.date_pill, None);
    }

    #[test]
    fn pending_bubble_carries_the_cmid() {
        let mut store = live_store();
        let _ = store.send("queued while reconnecting".into(), CMID, 0);
        let scene = build_scene(&store, &PresenceLog::default(), None, 0);
        let last = scene.messages.last().expect("pending bubble in scene");
        assert_eq!(last.id, CMID.to_string());
        assert_eq!(last.side, Side::Outgoing);
        assert!(!last.delivered);
    }

    // A pending bubble's scene id is its cmid, so the renderer's scene diff
    // catches the pending → delivered flip as remove+insert.
    #[test]
    fn ack_flips_scene_message_to_delivered() {
        let mut store = live_store();
        let cmid = new_cmid();
        let _ = store.send("hello".into(), cmid, 0);
        let _ = store.on_ack(cmid, "m_9".into(), 1);
        let scene = build_scene(&store, &PresenceLog::default(), None, 0);
        assert!(scene.messages.iter().all(|m| m.id != cmid.to_string()));
        assert_eq!(scene.messages.len(), 1);
        assert!(scene.messages[0].delivered);
    }

    // The typing-name lookup falls back to the raw id when the peer is gone.
    #[test]
    fn typing_name_falls_back_to_id() {
        let mut store = live_store();
        store.apply(
            Event::Frame(ServerFrame::Presence {
                join: Some(Peer {
                    id: "u_9".into(),
                    name: "Zed".into(),
                }),
                leave: None,
            }),
            0,
        );
        store.apply(
            Event::Frame(ServerFrame::Typing {
                user: "u_9".into(),
                on: true,
            }),
            0,
        );
        let scene = build_scene(&store, &PresenceLog::default(), None, 0);
        assert_eq!(scene.typing_peer.as_deref(), Some("Zed"));

        // Peer left: their typing state is cleared with them.
        store.apply(
            Event::Frame(ServerFrame::Presence {
                join: None,
                leave: Some("u_9".into()),
            }),
            0,
        );
        let scene = build_scene(&store, &PresenceLog::default(), None, 0);
        assert_eq!(scene.typing_peer, None);
    }

    // The shell forwards exactly the frames the store's effects name —
    // pinning the mapping here keeps the wire shape honest.
    #[test]
    fn send_effect_carries_the_wire_frame() {
        let mut store = live_store();
        let cmid = new_cmid();
        let effects = store.send("hi".into(), cmid, 0);
        assert!(effects.iter().any(
            |e| matches!(e, chat_core::Effect::SendMsg(ClientFrame::Msg { cmid: c, .. }) if *c == cmid)
        ));
    }
}
