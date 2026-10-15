#!/usr/bin/env sh
# Builds the phone client into web/dist/ (embedded into aic-relay at compile time).
# Needs: rustup target add wasm32-unknown-unknown, cargo install wasm-bindgen-cli (same version as Cargo.lock).
set -eu
cd "$(dirname "$0")"

if [ "$(uname)" = Darwin ] && [ -z "${AR_wasm32_unknown_unknown:-}" ]; then
  # Apple's ar can't index wasm archives; the llvm-ar from `rustup component add llvm-tools` can.
  AR_wasm32_unknown_unknown=$(ls "$HOME"/.rustup/toolchains/*/lib/rustlib/*/bin/llvm-ar 2>/dev/null | head -1)
  export AR_wasm32_unknown_unknown
fi

cargo build --target wasm32-unknown-unknown --release
target_dir=${CARGO_TARGET_DIR:-target}
wasm-bindgen --target web --no-typescript --out-dir dist "$target_dir/wasm32-unknown-unknown/release/aic_web.wasm"
cp static/index.html static/app.js dist/
ls -l dist
