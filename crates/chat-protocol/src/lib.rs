//! Wire protocol for the chat: JSON text frames tagged by `"t"`, exchanged
//! over the server's `/ws` WebSocket (spec art_d3wTgg6t §4).
//!
//! Invariants this crate will own once the frame types land (protocol PR):
//! - the server assigns a total order (`seq`); clients never order locally
//! - the client generates a `cmid` per send; retries are idempotent
//! - unknown server tags deserialize into a tolerant variant instead of
//!   failing the stream
//!
//! No wasm and no server-only dependencies — this crate compiles into both
//! the browser bundle and the axum host, natively tested on each PR.

// Intentionally empty for the scaffold: the serde enums arrive with the
// protocol PR. The dependency set above is the pin check.
