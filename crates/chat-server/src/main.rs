use std::io;

use chat_server::router;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> io::Result<()> {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    println!("chat-server listening on 0.0.0.0:{port}");
    axum::serve(listener, router()).await
}
