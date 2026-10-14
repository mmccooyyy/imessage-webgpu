#!/usr/bin/env bash
# Reproducible toolchain for this repository (spec art_d3wTgg6t §3).
#
# Installs, in order:
#   1. rustup + the stable toolchain (channel/targets/components come from
#      rust-toolchain.toml, so every rustup-driven rustc/cargo honors it)
#   2. rustfmt + clippy
#   3. the wasm32-unknown-unknown target
#   4. trunk 0.21.14 (pinned wasm bundler)
#   5. wasm-bindgen-cli 0.2.129 — must match the wasm-bindgen crate version
#      exactly, which the root Cargo.toml pins to =0.2.129
#
# The cargo install steps compile from source and take several minutes.
set -euo pipefail

if ! command -v rustup >/dev/null 2>&1; then
  echo "== installing rustup + stable toolchain =="
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  export PATH="$HOME/.cargo/bin:$PATH"
fi

# Explicit for environments where rust-toolchain.toml is not in the cwd.
rustup component add rustfmt clippy
rustup target add wasm32-unknown-unknown

echo "== installing trunk 0.21.14 (pinned) =="
cargo install --locked --version 0.21.14 trunk

echo "== installing wasm-bindgen-cli 0.2.129 (must match the wasm-bindgen crate) =="
cargo install --locked --version 0.2.129 wasm-bindgen-cli

echo "== toolchain ready =="
rustc --version
cargo --version
trunk --version
wasm-bindgen --version
