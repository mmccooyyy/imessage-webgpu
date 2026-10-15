#!/usr/bin/env bash
# Build the dual-bundle dist/ the client serves in prod (spec art_d3wTgg6t
# §2.4, §4.1):
#   dist/webgpu/   — default-features wasm bundle (wgpu BROWSER_WEBGPU)
#   dist/webgl2/   --features webgl bundle (wgpu GL backend)
#   dist/index.html— capability chooser: navigator.gpu → webgpu bundle,
#                    else the WebGL2 bundle; a failed WebGPU load falls back.
# Each bundle dir keeps trunk's own self-booting index.html for deep links.
# Run from the repo root: scripts/build-dist.sh
set -euo pipefail
cd "$(dirname "$0")/.."

command -v trunk >/dev/null || { echo "trunk not on PATH" >&2; exit 1; }

echo "==> building WebGPU bundle"
trunk build --release --dist dist/webgpu --public-url /webgpu/

echo "==> building WebGL2 fallback bundle"
trunk build --release --features webgl --dist dist/webgl2 --public-url /webgl2/

entry_webgpu=$(ls dist/webgpu | grep -E '^chat-client-.*\.js$' | head -1)
entry_webgl2=$(ls dist/webgl2 | grep -E '^chat-client-.*\.js$' | head -1)
[[ -n "$entry_webgpu" && -n "$entry_webgl2" ]] || {
  echo "trunk produced no chat-client-*.js entries" >&2
  exit 1
}

# Root chooser page (spec art_d3wTgg6t §2.4): feature-detect navigator.gpu
# and redirect to the matching bundle's own page — each bundle's trunk
# index.html carries the full DOM overlay (nav bar, join card, input) the
# wasm app requires, and handles a failed adapter by redirecting to the
# other bundle (sessionStorage-guarded, one attempt).
cat > dist/index.html <<HTML
<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover" />
    <title>iMessage</title>
  </head>
  <body>
    <script>
      location.replace(navigator.gpu ? '/webgpu/' : '/webgl2/');
    </script>
  </body>
</html>
HTML

echo "==> dist assembled:"
find dist -maxdepth 2 -type f | sort | sed 's/^/    /'
