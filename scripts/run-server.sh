#!/usr/bin/env bash
# Build and serve the production topology (spec art_d3wTgg6t §3): one axum
# process serving the trunk-built dist/ and the /ws WebSocket from a single
# origin. Used locally and by the hosted-deploy flow (task todo_VndyVyJP).
#
# Usage:
#   scripts/run-server.sh              # build dist/ + server, then serve
#   scripts/run-server.sh --serve-only # skip builds; serve the existing build
#
# Environment:
#   PORT   port chat-server binds (default 8080); passed through untouched.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BUILD=1
if [[ "${1:-}" == "--serve-only" ]]; then
  BUILD=0
elif [[ -n "${1:-}" ]]; then
  echo "usage: $0 [--serve-only]" >&2
  exit 2
fi

if [[ "$BUILD" == 1 ]]; then
  echo "== building client bundle (trunk → dist/) =="
  trunk build --release

  echo "== building server (cargo release) =="
  cargo build --release -p chat-server
fi

if [[ ! -x target/release/chat-server ]]; then
  echo "target/release/chat-server missing — run once without --serve-only" >&2
  exit 1
fi
if [[ ! -f dist/index.html ]]; then
  echo "dist/index.html missing — run once without --serve-only" >&2
  exit 1
fi

export PORT="${PORT:-8080}"
echo "== serving dist/ + /ws on 0.0.0.0:${PORT} =="
exec ./target/release/chat-server
