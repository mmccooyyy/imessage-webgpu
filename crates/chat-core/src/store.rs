//! The conversation store: a pure reducer over socket events, decoded
//! server frames, and viewport state (spec art_d3wTgg6t §5).
//!
//! Every mutator is a function of `(state, input, injected clock) → (state,
//! effects)`. Time never enters on its own: [`Store::apply`], [`Store::send`],
//! and [`Store::tick`] take the store clock in milliseconds from the caller,
//! and typing deadlines / reconnect timers are plain `u64`s against it.
//! [`Effect`]s are instructions for the browser shell — the store itself
//! never touches the socket, the DOM, wgpu, or a clock, which is what keeps
//! this crate natively testable and free of web-sys and tokio.
//!
//! Ordering and dedupe rules the reducer preserves:
//! - messages render in server-`seq` order only; a gap requests `resume`
//!   instead of rendering ahead of the hole
//! - `cmid` is idempotent: a duplicate send or a duplicate ack never
//!   renders twice
//! - typing indicators expire client-side ~3 s after the last refresh

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use chat_protocol::{ClientFrame, Msg, Peer, ServerFrame};
use uuid::Uuid;

use crate::backoff::backoff;

/// A typing indicator dies after this long without a refresh (spec: ~3 s).
pub const TYPING_TIMEOUT_MS: u64 = 3_000;

/// The connection lifecycle the shell renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnState {
    /// Socket opening or `hello` sent, `welcome` not yet applied.
    Connecting,
    /// The first connection failed before any `welcome` arrived; the shell
    /// shows a failed card and the user retries manually.
    ConnectingFailed,
    /// Live: joined, history applied, frames flowing.
    Connected,
    /// Lost a live connection; auto-retrying with exponential backoff.
    /// `tick` fires the retry at `next_retry` (store-clock millis).
    Reconnecting { attempt: u32, next_retry: u64 },
    /// Reconnected and caught up after a drop — same semantics as
    /// [`ConnState::Connected`], kept distinct so the shell can clear the
    /// reconnect banner differently.
    OfflineResumed,
}

impl ConnState {
    /// True where the session behaves as live (send + render traffic).
    pub fn is_live(&self) -> bool {
        matches!(self, ConnState::Connected | ConnState::OfflineResumed)
    }
}

/// A side effect the shell must run after a store transition. The store
/// never performs I/O itself; effects are its whole output vocabulary.
/// The shell treats `Render`/`Scroll` as idempotent.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Open the WebSocket and send `hello` (the shell owns the display name).
    Connect,
    /// Transmit a client frame over the socket (queueing while disconnected
    /// is the shell's job; the store re-emits pending sends on `welcome`).
    SendMsg(ClientFrame),
    /// Ask the server to replay everything after `last_seq`, in seq order.
    Resume { last_seq: u64 },
    /// Arm the reconnect timer for this long (informational — `tick` is the
    /// authority that actually fires the retry).
    RetryAfter(Duration),
    /// Repaint the canvas.
    Render,
    /// Scroll the conversation to the bottom.
    Scroll,
}

/// Inputs that change the conversation without a local user intent.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The WebSocket opened (initially or on a retry).
    SocketOpened,
    /// The WebSocket closed or errored. `jitter` in [0, 1] seeds the backoff
    /// spread — passed in because the store draws no randomness itself.
    SocketClosed { jitter: f32 },
    /// A decoded server frame. Unknown tags were already absorbed into the
    /// tolerant `AnyFrame` variant by the protocol layer and never get here.
    Frame(ServerFrame),
    /// The user scrolled the conversation; `at_bottom` is whether the
    /// viewport sits at (or past) the bottom edge.
    ViewportScrolled { at_bottom: bool },
}

/// An optimistic, not-yet-acknowledged outgoing message ("sending…" bubble).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMsg {
    pub cmid: Uuid,
    pub text: String,
    /// Store-clock millis when the send was queued.
    pub queued_at: u64,
}

/// A delivered, server-ordered message as the renderer consumes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub id: String,
    pub from: String,
    pub name: String,
    pub text: String,
    /// Server epoch seconds for peer messages; `0` for our own (the ack
    /// carries no timestamp, and v1 renders no per-bubble time).
    pub ts: i64,
    pub seq: u64,
    /// Our own message — right-aligned blue bubble.
    pub mine: bool,
}

/// The whole conversation state. Every mutator is pure: `(state, input,
/// clock) → (state, effects)`.
#[derive(Debug, Clone)]
pub struct Store {
    me: Option<String>,
    conn: ConnState,
    /// Whether any `welcome` ever landed — separates first-contact failure
    /// (`ConnectingFailed`) from a drop of a live session (reconnect loop).
    ever_connected: bool,
    /// Retries already scheduled for the current outage; survives the
    /// `Reconnecting → Connecting` hand-off so a failed retry re-arms
    /// with a longer delay.
    reconnect_attempt: u32,
    /// Delivered messages, sorted ascending by `seq`.
    messages: Vec<ChatMessage>,
    /// Unacked sends, in send order (the "sending…" tail of the transcript).
    pending: Vec<PendingMsg>,
    /// Joined peers by user id.
    peers: BTreeMap<String, Peer>,
    /// Peer id → typing deadline (store-clock millis).
    typing: HashMap<String, u64>,
    /// Highest contiguous applied `seq`.
    last_seq: u64,
    /// Stick to the bottom on new content unless the user scrolled up.
    stick_to_bottom: bool,
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Store {
    /// A fresh store: `Connecting`, empty history, sticky scroll.
    pub fn new() -> Self {
        Store {
            me: None,
            conn: ConnState::Connecting,
            ever_connected: false,
            reconnect_attempt: 0,
            messages: Vec::new(),
            pending: Vec::new(),
            peers: BTreeMap::new(),
            typing: HashMap::new(),
            last_seq: 0,
            stick_to_bottom: true,
        }
    }

    pub fn conn(&self) -> &ConnState {
        &self.conn
    }

    pub fn is_live(&self) -> bool {
        self.conn.is_live()
    }

    pub fn me(&self) -> Option<&str> {
        self.me.as_deref()
    }

    /// Delivered messages, ascending by `seq`.
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Unacked sends, in send order.
    pub fn pending(&self) -> &[PendingMsg] {
        &self.pending
    }

    /// Currently joined peers.
    pub fn peers(&self) -> impl Iterator<Item = &Peer> {
        self.peers.values()
    }

    /// Peer ids whose typing indicator is alive at `now` (deadline is
    /// exclusive: an indicator set at `t` dies at `t + TYPING_TIMEOUT_MS`).
    pub fn typing_users(&self, now: u64) -> Vec<String> {
        self.typing
            .iter()
            .filter(|(_, &deadline)| now < deadline)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Highest contiguous applied `seq`.
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    pub fn stick_to_bottom(&self) -> bool {
        self.stick_to_bottom
    }

    /// User intent: send `text`. Optimistically queues a pending bubble and
    /// asks the shell to transmit `ClientFrame::Msg` with the caller-supplied
    /// `cmid` — retries on reconnect reuse the same `cmid`, so the server
    /// dedupes and redelivery never renders twice.
    ///
    /// A no-op while the input is inert (`Connecting` / `ConnectingFailed`)
    /// or when this `cmid` is already pending.
    pub fn send(&mut self, text: String, cmid: Uuid, now: u64) -> Vec<Effect> {
        let sends_accepted =
            self.conn.is_live() || matches!(self.conn, ConnState::Reconnecting { .. });
        if !sends_accepted || self.pending.iter().any(|p| p.cmid == cmid) {
            return Vec::new();
        }
        let frame = ClientFrame::Msg {
            cmid,
            text: text.clone(),
        };
        self.pending.push(PendingMsg {
            cmid,
            text,
            queued_at: now,
        });
        // An explicit send is an intent to watch the reply: stick to bottom.
        self.stick_to_bottom = true;
        vec![Effect::SendMsg(frame), Effect::Render, Effect::Scroll]
    }

    /// Delivery confirmation for our own send: retire the pending bubble and
    /// file the message as delivered at its server-assigned place in the
    /// total order. Unknown or already-acked `cmid`s are a no-op.
    pub fn on_ack(&mut self, cmid: Uuid, id: String, seq: u64) -> Vec<Effect> {
        let Some(pending) = self
            .pending
            .iter()
            .position(|p| p.cmid == cmid)
            .map(|index| self.pending.remove(index))
        else {
            return Vec::new();
        };
        let me = self.me.clone().unwrap_or_default();
        self.file_message(ChatMessage {
            id,
            from: me.clone(),
            name: me,
            text: pending.text,
            ts: 0,
            seq,
            mine: true,
        });
        let mut effects = vec![Effect::Render];
        if seq == self.last_seq + 1 {
            self.last_seq = seq;
        } else if seq > self.last_seq + 1 {
            // Our message skipped ahead of missed peer broadcasts — request
            // the hole; the replay's echo of this very message is deduped.
            effects.push(Effect::Resume {
                last_seq: self.last_seq,
            });
        }
        effects
    }

    /// Clock injection: expire typing indicators and fire the reconnect
    /// timer. Called once per animation frame (or coarser — the shell's
    /// cadence is the expiry granularity).
    pub fn tick(&mut self, now: u64) -> Vec<Effect> {
        let mut effects = Vec::new();

        // Reconnect due? tick is the authority that fires retries, and it
        // transitions to Connecting first so a repeat tick cannot re-fire.
        let retry_due = matches!(
            self.conn,
            ConnState::Reconnecting { next_retry, .. } if now >= next_retry
        );
        if retry_due {
            self.conn = ConnState::Connecting;
            effects.push(Effect::Connect);
        }

        let expired: Vec<String> = self
            .typing
            .iter()
            .filter(|(_, &deadline)| now >= deadline)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            self.typing.remove(id);
        }
        // Render only when something visually changed: a retry fired, or at
        // least one indicator expired this tick.
        if retry_due || !expired.is_empty() {
            effects.push(Effect::Render);
        }
        effects
    }

    /// Socket lifecycle, server frames, and viewport updates.
    pub fn apply(&mut self, event: Event, now: u64) -> Vec<Effect> {
        match event {
            Event::SocketOpened => {
                // A live session never sees a second open; guard anyway.
                if !self.conn.is_live() {
                    self.conn = ConnState::Connecting;
                }
                Vec::new()
            }
            Event::SocketClosed { jitter } => self.on_socket_closed(jitter, now),
            Event::ViewportScrolled { at_bottom } => {
                if self.stick_to_bottom == at_bottom {
                    Vec::new()
                } else {
                    self.stick_to_bottom = at_bottom;
                    vec![Effect::Render]
                }
            }
            Event::Frame(frame) => self.apply_frame(frame, now),
        }
    }

    fn on_socket_closed(&mut self, jitter: f32, now: u64) -> Vec<Effect> {
        if !self.ever_connected {
            // First contact never completed: user-visible failure, manual
            // retry (the shell re-opens the socket, which lands as
            // SocketOpened → Connecting).
            if self.conn != ConnState::ConnectingFailed {
                self.conn = ConnState::ConnectingFailed;
                return vec![Effect::Render];
            }
            return Vec::new();
        }
        // Drop of a live (or reconnecting) session: re-arm the backoff.
        self.reconnect_attempt += 1;
        let delay = backoff(self.reconnect_attempt - 1, jitter);
        self.conn = ConnState::Reconnecting {
            attempt: self.reconnect_attempt - 1,
            next_retry: now.saturating_add(delay.as_millis() as u64),
        };
        vec![Effect::RetryAfter(delay), Effect::Render]
    }

    fn apply_frame(&mut self, frame: ServerFrame, now: u64) -> Vec<Effect> {
        match frame {
            ServerFrame::Welcome {
                you,
                users,
                history,
                seq,
            } => self.apply_welcome(you, users, history, seq),
            ServerFrame::Msg {
                id,
                from,
                name,
                text,
                ts,
                seq,
            } => self.apply_msg(Msg {
                id,
                from,
                name,
                text,
                ts,
                seq,
            }),
            ServerFrame::Ack { cmid, id, seq } => self.on_ack(cmid, id, seq),
            ServerFrame::Typing { user, on } => self.apply_typing(user, on, now),
            ServerFrame::Presence { join, leave } => {
                let mut effects = Vec::new();
                if let Some(peer) = join {
                    self.peers.entry(peer.id.clone()).or_insert(peer);
                    effects.push(Effect::Render);
                }
                if let Some(id) = leave {
                    if self.peers.remove(&id).is_some() {
                        self.typing.remove(&id);
                        effects.push(Effect::Render);
                    }
                }
                effects
            }
            ServerFrame::Error { .. } => {
                // Protocol-level rejection (e.g. no_hello). Before the first
                // welcome it means the join failed; afterwards, ignore — the
                // demo protocol defines no recoverable server errors.
                if self.conn == ConnState::Connecting && !self.ever_connected {
                    self.conn = ConnState::ConnectingFailed;
                    return vec![Effect::Render];
                }
                Vec::new()
            }
        }
    }

    fn apply_welcome(
        &mut self,
        you: String,
        users: Vec<Peer>,
        history: Vec<Msg>,
        seq: u64,
    ) -> Vec<Effect> {
        let prior_conn = self.conn.clone();
        let prior_last_seq = self.last_seq;
        let resumed = self.ever_connected;

        self.me = Some(you.clone());
        self.peers = users.into_iter().map(|p| (p.id.clone(), p)).collect();
        self.messages = history
            .into_iter()
            .map(|m| ChatMessage {
                mine: m.from == you,
                id: m.id,
                from: m.from,
                name: m.name,
                text: m.text,
                ts: m.ts,
                seq: m.seq,
            })
            .collect();
        self.messages.sort_unstable_by_key(|m| m.seq);
        // Adopt the server's cursor unconditionally: a resumed session with a
        // server that rebooted (seq reset) self-heals to the room's truth.
        self.last_seq = seq;
        self.typing.clear(); // typing state does not survive a socket
        self.ever_connected = true;
        self.reconnect_attempt = 0;
        self.conn = if !resumed {
            ConnState::Connected
        } else if matches!(
            prior_conn,
            ConnState::Connecting | ConnState::Reconnecting { .. }
        ) {
            ConnState::OfflineResumed
        } else {
            prior_conn
        };

        let mut effects = vec![Effect::Render];
        if resumed && seq > prior_last_seq {
            // Spec: every successful reconnect sends resume. The replay may
            // overlap the welcome history; apply_msg dedupes by seq.
            effects.push(Effect::Resume {
                last_seq: prior_last_seq,
            });
        }
        // Re-queue unacked sends on the fresh socket — same cmids, so the
        // server dedupes anything that actually landed before the drop.
        for pending in &self.pending {
            effects.push(Effect::SendMsg(ClientFrame::Msg {
                cmid: pending.cmid,
                text: pending.text.clone(),
            }));
        }
        if self.stick_to_bottom {
            effects.push(Effect::Scroll);
        }
        effects
    }

    fn apply_msg(&mut self, msg: Msg) -> Vec<Effect> {
        // Everything at or before the cursor was already rendered.
        if msg.seq <= self.last_seq {
            return Vec::new();
        }
        let Msg {
            id,
            from,
            name,
            text,
            ts,
            seq,
        } = msg;
        match self.messages.binary_search_by_key(&seq, |m| m.seq) {
            // Our own ack'd message echoed back by a replay: already on
            // screen — just extend the cursor over it.
            Ok(_) => {
                self.last_seq = seq;
                Vec::new()
            }
            Err(index) => {
                if seq != self.last_seq + 1 {
                    // Gap: never render out of order — request the hole and
                    // drop this frame; the replay re-delivers it in order.
                    return vec![Effect::Resume {
                        last_seq: self.last_seq,
                    }];
                }
                let mine = Some(from.as_str()) == self.me.as_deref();
                self.messages.insert(
                    index,
                    ChatMessage {
                        id,
                        from,
                        name,
                        text,
                        ts,
                        seq,
                        mine,
                    },
                );
                self.last_seq = seq;
                let mut effects = vec![Effect::Render];
                if self.stick_to_bottom {
                    effects.push(Effect::Scroll);
                }
                effects
            }
        }
    }

    fn apply_typing(&mut self, user: String, on: bool, now: u64) -> Vec<Effect> {
        if Some(user.as_str()) == self.me.as_deref() {
            return Vec::new(); // the server never echoes own typing; guard anyway
        }
        if on {
            let already_typing = self.typing.contains_key(&user);
            self.typing
                .insert(user, now.saturating_add(TYPING_TIMEOUT_MS));
            if already_typing {
                Vec::new() // a refresh changes no visuals
            } else {
                vec![Effect::Render]
            }
        } else if self.typing.remove(&user).is_some() {
            vec![Effect::Render]
        } else {
            Vec::new()
        }
    }

    /// Sorted insertion keeping `messages` ascending by `seq`.
    fn file_message(&mut self, message: ChatMessage) {
        let index = self.messages.partition_point(|m| m.seq < message.seq);
        self.messages.insert(index, message);
    }
}
