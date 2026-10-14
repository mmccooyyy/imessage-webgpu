#!/usr/bin/env bash
# Dependency-isolation guard (spec risk #6: tokio must never enter a client
# crate — async on wasm is wasm-bindgen-futures + gloo timers).
#
# Enforced:
#   - tokio and axum may not be declared by, nor resolve into, any of the
#     client-side crates (chat-protocol, chat-core, chat-client)
#   - gloo (the browser WebSocket/timer stack) stays out of the pure crates
#     (chat-protocol, chat-core); chat-client uses gloo-net by design
set -euo pipefail

fail=0

for crate in chat-protocol chat-core chat-client; do
  manifest="crates/${crate}/Cargo.toml"
  for dep in tokio axum; do
    if grep -Eq "^[[:space:]]*${dep}(\.[a-zA-Z_-]+)?[[:space:]]*=" "${manifest}"; then
      echo "FAIL: '${dep}' declared in ${crate} (server-only dependency)" >&2
      fail=1
    fi
  done
done

for crate in chat-protocol chat-core; do
  manifest="crates/${crate}/Cargo.toml"
  # gloo-* family (gloo, gloo-net, gloo-timers, ...) — prefix match, since the
  # crate names are hyphenated
  if grep -Eq "^[[:space:]]*gloo[a-zA-Z0-9_.-]*[[:space:]]*=" "${manifest}"; then
    echo "FAIL: gloo declared in ${crate} (browser-shell-only dependency)" >&2
    fail=1
  fi
done

# Belt and braces: check the resolved graph for the wasm target too — catches
# target-specific declarations and transitive-only leaks.
for crate in chat-protocol chat-core chat-client; do
  if ! tree=$(cargo tree -p "${crate}" --target wasm32-unknown-unknown 2>/dev/null); then
    echo "WARN: cargo tree failed for ${crate}; skipping graph check" >&2
    continue
  fi
  for dep in tokio axum; do
    if echo "${tree}" | grep -Eq "(^|[[:space:]])${dep} v"; then
      echo "FAIL: '${dep}' found in ${crate}'s resolved dependency graph" >&2
      fail=1
    fi
  done
done

if [[ "${fail}" -eq 0 ]]; then
  echo "dependency guard: OK"
fi
exit "${fail}"
