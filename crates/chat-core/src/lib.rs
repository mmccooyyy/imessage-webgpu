//! Client state machine (spec art_d3wTgg6t §5): the platform-independent
//! correctness core behind the iMessage conversation surface.
//!
//! The [`store::Store`] owns the whole conversation — connection state,
//! seq-ordered messages, pending sends keyed by `cmid`, typing deadlines,
//! and scroll stickiness — as a pure reducer: every mutator takes an input
//! plus an injected clock and returns [`store::Effect`]s for the shell to
//! run. The store never touches the socket, the DOM, wgpu, or a clock, so
//! the whole thing runs under plain `cargo test`.
//!
//! Dependency discipline (CI-enforced): no web-sys, no tokio, no gloo —
//! only [`chat_protocol`] wire types and `uuid` for the `cmid` type.

pub mod backoff;
pub mod store;

pub use backoff::backoff;
pub use store::{ChatMessage, ConnState, Effect, Event, PendingMsg, Store, TYPING_TIMEOUT_MS};
