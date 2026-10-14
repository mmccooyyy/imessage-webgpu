#!/usr/bin/env bash
# Local smoke test: build chat-server, boot it, curl /healthz, assert 200 "ok".
set -euo pipefail

PORT="${PORT:-8080}"

if curl -sS -o /dev/null "http://127.0.0.1:${PORT}/healthz" 2>/dev/null; then
  echo "port ${PORT} already serving /healthz — aborting rather than testing the wrong server" >&2
  exit 1
fi

cargo build -p chat-server

./target/debug/chat-server &
server_pid=$!
trap 'kill "${server_pid}" 2>/dev/null || true' EXIT

for _ in $(seq 1 50); do
  if curl -sS -o /dev/null "http://127.0.0.1:${PORT}/healthz" 2>/dev/null; then
    break
  fi
  sleep 0.2
done

code=$(curl -sS -o /dev/null -w '%{http_code}' "http://127.0.0.1:${PORT}/healthz")
body=$(curl -fsS "http://127.0.0.1:${PORT}/healthz")

if [[ "${code}" == "200" && "${body}" == "ok" ]]; then
  echo "smoke ok: GET /healthz -> ${code} ${body}"
else
  echo "smoke FAILED: GET /healthz -> ${code} ${body}" >&2
  exit 1
fi
