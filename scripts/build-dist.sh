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

# Root chooser page (spec art_d3wTgg6t §2.4): feature-detect navigator.gpu,
# import the matching bundle's entry (each entry self-boots on import), and
# swap to the other bundle if the first fails to load — navigator.gpu can
# exist while adapter acquisition still fails (MDN). One retry, then stop.
cat > dist/index.html <<HTML
<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover" />
    <title>iMessage</title>
    <style>
      html, body { margin: 0; height: 100%; background: #fff; overflow: hidden; }
      #app { display: block; width: 100%; height: 100%; touch-action: none; }
      #overlay {
        position: fixed; inset: auto 0 0 0; padding: 8px 16px;
        font: 14px/1.4 system-ui, sans-serif; color: #6b7280;
        pointer-events: none; user-select: none;
      }
    </style>
  </head>
  <body>
    <canvas id="app" aria-label="conversation canvas"></canvas>
    <div id="overlay"></div>
    <script type="module">
      const entries = {
        webgpu: '/webgpu/${entry_webgpu}',
        webgl2: '/webgl2/${entry_webgl2}',
      };
      // trunk's entry default-exports the wasm-bindgen init fn — call it
      // with the sibling .wasm path, or nothing runs (silent blank canvas).
      const boot = async (which) => {
        const mod = await import(entries[which]);
        await mod.default({ module_or_path: entries[which].replace(/\.js$/, '_bg.wasm') });
      };
      const first = navigator.gpu ? 'webgpu' : 'webgl2';
      const second = navigator.gpu ? 'webgl2' : 'webgpu';
      boot(first).catch((err) => {
        console.error(first + ' bundle failed to load:', err);
        boot(second).catch((e2) =>
          console.error(second + ' bundle failed too:', e2),
        );
      });
    </script>
  </body>
</html>
HTML

echo "==> dist assembled:"
find dist -maxdepth 2 -type f | sort | sed 's/^/    /'
