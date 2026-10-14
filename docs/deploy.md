# Deploying the chat server

One axum process serves the page (`dist/`) and the `/ws` WebSocket from a
single origin (spec art_d3wTgg6t §3, decision "Serving topology"). The page
derives `ws://` vs `wss://` from `location.protocol` — no host is ever
hard-coded, so the same build runs locally and hosted.

## Run locally

```sh
./scripts/install-toolchain.sh   # once: rustup stable, wasm32, trunk, wasm-bindgen-cli
./scripts/run-server.sh          # trunk build → dist/, cargo build -p chat-server, serve :8080
```

Then open http://localhost:8080. `/healthz` returns `ok`.

`scripts/run-server.sh --serve-only` skips the builds and serves the existing
`dist/` + `target/release/chat-server` — used when restarting a deployment
where the toolchain is not installed (e.g. the hosting sandbox).

## Hosted deploy (persistent URL)

Builds happen on the repo sandbox; hosting happens on the project sandbox.
Port 8080 is the canonical port, and the tmux session **must** be named
`svc-8080` (the hosting wake path restarts services under `svc-${PORT}`).

1. **Build on the repo sandbox** (toolchain installed there):

   ```sh
   trunk build --release && cargo build --release -p chat-server
   ```

2. **Copy the built artifacts** to the project sandbox, preserving the layout
   `run-server.sh --serve-only` expects (`target/release/chat-server` +
   `dist/` next to the repo's `scripts/`):

   ```sh
   obvious computers copy \
     --from cmp_AuEWSEA8:/home/user/work/imessage-webgpu/dist \
     --to   obvious:/home/user/work/imessage-hosted/dist
   obvious computers copy \
     --from cmp_AuEWSEA8:/home/user/work/imessage-webgpu/target/release/chat-server \
     --to   obvious:/home/user/work/imessage-hosted/target/release/chat-server
   obvious computers copy \
     --from cmp_AuEWSEA8:/home/user/work/imessage-webgpu/scripts/run-server.sh \
     --to   obvious:/home/user/work/imessage-hosted/scripts/run-server.sh
   chmod +x /home/user/work/imessage-hosted/scripts/run-server.sh
   ```

3. **Start under the canonical tmux name and verify locally first:**

   ```sh
   tmux new-session -d -s svc-8080 \
     "PORT=8080 /home/user/work/imessage-hosted/scripts/run-server.sh --serve-only \
        2>&1 | tee /home/user/work/logs/svc-8080.log"
   for i in {1..15}; do curl -sf localhost:8080/healthz && break; sleep 1; done
   ```

4. **Register for a persistent URL** (wake-on-request restarts the same
   command under `svc-8080`):

   ```sh
   obvious hosted register --port=8080 \
     --startup-command="PORT=8080 /home/user/work/imessage-hosted/scripts/run-server.sh --serve-only" \
     --work-dir=/home/user/work/imessage-hosted
   ```

   Share the returned `hostedUrl` — never the `directUrl`.

## Verifying the WebSocket upgrade through the hosted edge

The hosted edge terminates TLS in front of the sandbox; the open question
(spec risk #3) is whether it forwards `Upgrade`/`Connection` headers. Verify
after every deploy:

```sh
# Expect "HTTP/1.1 101 Switching Protocols" (curl holds the socket open until
# --max-time; the status line is what matters):
curl -i -N --max-time 5 https://<hosted-host>/ws \
  -H "Connection: Upgrade" -H "Upgrade: websocket" \
  -H "Sec-WebSocket-Version: 13" -H "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ=="
```

And from a real browser (console):

```js
const ws = new WebSocket("wss://<hosted-host>/ws");
ws.onopen = () => ws.send(JSON.stringify({ t: "hello", name: "spike" }));
ws.onmessage = (e) => console.log("frame:", e.data);
// Expect frame: {"t":"welcome",...}
```

## If the edge refuses upgrades (fallback topology)

Documented fallback (spec risk #3): host the static client via the hosting
flow and serve `/ws` from the server's own origin, re-pointing the client's
ws URL through a build-time env (e.g. `TRUNK_WS_ORIGIN`). Nothing about the
four-crate architecture changes; only the client's URL derivation gains one
build-time override. Record the topology change in the deploy PR before
integration work (todo_9is44aoE) consumes it.

> Spike verdict (2026-10-14, verified against the live hosted URL
> https://063rvk6mz2-8080.hosted.obvious.ai): **single-origin works** — the
> hosted edge forwards WebSocket upgrades. `GET /` serves `dist/`,
> `/healthz` → `ok`, and an HTTP/1.1 upgrade request to `/ws` returns
> `101 Switching Protocols`; a browser `wss://` socket completed
> hello → `{"t":"welcome",...}` (screenshot evidence in PR #5). No fallback
> needed; integration (todo_9is44aoE) can rely on the single-origin topology.
>
> Curl caveat: over HTTP/2 the edge answers a raw `Upgrade` request with
> `400` (h2 cannot carry a plain Upgrade) — verify with `curl --http1.1`.
> Browsers negotiate `wss://` over HTTP/1.1, so this does not affect the app.

