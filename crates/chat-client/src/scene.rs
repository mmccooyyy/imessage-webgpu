//! Plain message data that drives the renderer (spec art_d3wTgg6t §5).
//!
//! The later chat-core integration PR produces a `Scene` per frame; the
//! renderer knows nothing about sockets, the store, or the DOM overlay.

// The host build exists to run these modules' tests; the wasm renderer
// consumes the rest of this module's items.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

/// Which side of the conversation a message sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Peer message: gray bubble, left-aligned, tail at bottom-left.
    Incoming,
    /// My message: iOS-blue bubble, right-aligned, tail at bottom-right.
    Outgoing,
}

/// One chat message bubble.
#[derive(Clone, Debug)]
pub struct Message {
    /// Stable id — used as the diff key between scenes (dirty detection).
    pub id: String,
    pub side: Side,
    pub text: String,
    /// Outgoing only: ack arrived → "Delivered" under the group's last bubble.
    pub delivered: bool,
}

/// Connection banner shown over the conversation surface. `Connecting`/
/// `Reconnecting` are constructed once chat-core integration lands (next PR).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Banner {
    Connecting,
    Reconnecting,
}

/// Everything the renderer draws for one frame, in conversation order.
#[derive(Clone, Debug, Default)]
pub struct Scene {
    /// Conversation-start pill label (e.g. "Today 4:12 PM"). `None` hides it.
    pub date_pill: Option<String>,
    /// Transcript pills stacked under the date pill: presence events
    /// ("Sam joined" / "Sam left"), in order.
    pub pills: Vec<String>,
    pub messages: Vec<Message>,
    /// Display name of the peer currently typing, if any.
    pub typing_peer: Option<String>,
    pub banner: Option<Banner>,
}

impl Scene {
    /// True when nothing a frame would change differs from `other`.
    /// The renderer compares the last laid-out scene against this to decide
    /// whether the frame is dirty (typing-dot animation aside).
    pub fn same_as(&self, other: &Scene) -> bool {
        self.date_pill == other.date_pill
            && self.pills == other.pills
            && self.typing_peer == other.typing_peer
            && self.banner == other.banner
            && self.messages.len() == other.messages.len()
            && self
                .messages
                .iter()
                .zip(other.messages.iter())
                .all(|(a, b)| {
                    a.id == b.id
                        && a.side == b.side
                        && a.text == b.text
                        && a.delivered == b.delivered
                })
    }

    /// The spec-mock conversation (spec art_d3wTgg6t §5): proves shaped text,
    /// grouped bubbles, tails, "Delivered", emoji fallback, and typing dots
    /// before any socket exists.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub fn demo() -> Self {
        Scene {
            pills: Vec::new(),
            date_pill: Some("Today 4:12 PM".to_string()),
            messages: vec![
                Message {
                    id: "d1".into(),
                    side: Side::Incoming,
                    text: "yo is the two-browser demo ready for Friday? 🚀".into(),
                    delivered: false,
                },
                Message {
                    id: "d2".into(),
                    side: Side::Incoming,
                    text: "judges walk the tables at 3".into(),
                    delivered: false,
                },
                Message {
                    id: "d3".into(),
                    side: Side::Outgoing,
                    text: "shipping it tonight — open two tabs, chat live".into(),
                    delivered: true,
                },
                Message {
                    id: "d4".into(),
                    side: Side::Incoming,
                    text: "what's the quality bar?".into(),
                    delivered: false,
                },
                Message {
                    id: "d5".into(),
                    side: Side::Outgoing,
                    text: "feels like Messages, not works-somehow 🎉".into(),
                    delivered: true,
                },
            ],
            typing_peer: Some("Sam".to_string()),
            banner: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_scene_compares_equal() {
        let a = Scene::demo();
        let mut b = Scene::demo();
        assert!(a.same_as(&b));
        b.messages[2].delivered = false;
        assert!(!a.same_as(&b));
    }

    #[test]
    fn different_length_compares_unequal() {
        let a = Scene::demo();
        let mut b = Scene::demo();
        b.messages.pop();
        assert!(!a.same_as(&b));
    }
}
