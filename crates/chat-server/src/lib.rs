//! Axum server (spec art_d3wTgg6t §3): one process serves the client bundle
//! from `dist/`, upgrades `/ws`, and answers `/healthz`. Room state — joined
//! users, a ~200-message history ring, a monotonically increasing `seq` —
//! arrives with the server PR behind a mutex.

use axum::{routing::get, Router};
use tower_http::services::ServeDir;

/// Build the router. Separate from `main` so tests can drive it without a socket.
pub fn router() -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .fallback_service(ServeDir::new("dist/"))
}

async fn healthz() -> &'static str {
    "ok"
}
