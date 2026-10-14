//! Client state machine (spec art_d3wTgg6t §5): the store and reducer behind
//! the four connection states (Connecting, Live, Reconnecting, Peer typing)
//! and two message states (pending, delivered), reconnect backoff math
//! (0.5 s × 2^attempt, cap 30 s, ±20% jitter), and bubble-layout math.
//!
//! Deliberately free of web-sys, wgpu, and tokio so the correctness core runs
//! under plain `cargo test`; the browser shell in `chat-client` drives it.

// Intentionally empty for the scaffold: the store, reducer, and backoff land
// with the chat-core PR; the dependency edge on chat-protocol is established
// here so downstream PRs touch only crate-local files.
