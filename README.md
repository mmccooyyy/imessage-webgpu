# imessage-webgpu

iMessage-style chat rendered entirely with WebGPU from Rust compiled to wasm;
an axum server hosts the page and the `/ws` WebSocket from one origin.

Four-crate cargo workspace:

| crate | role |
| --- | --- |
| `crates/chat-protocol` | wire types shared by client and server |
| `crates/chat-core` | client state machine — native-testable, no wasm deps |
| `crates/chat-client` | browser shell: wgpu + glyphon renderer, DOM overlay entry |
| `crates/chat-server` | axum: `/ws`, static `dist/`, `/healthz` |

## Toolchain

```sh
./scripts/install-toolchain.sh   # rustup stable, wasm32 target, trunk 0.21.14, wasm-bindgen-cli 0.2.129
trunk build --release            # client bundle → dist/
cargo run -p chat-server         # serves dist/ + /ws on :8080
```

CI runs fmt, clippy `-D warnings`, workspace tests, a dependency-isolation
guard (no tokio/axum in client crates), and trunk builds for both the WebGPU
bundle and the WebGL2 fallback bundle.
