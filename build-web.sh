#!/usr/bin/env bash
set -euo pipefail

# ──────────────────────────────────────────────────────────────────
# build-web.sh — Build the roguelike for WASM and package for itch.io
#
# Prerequisites:
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli
#   (optional) cargo install wasm-opt   # for smaller .wasm files
#
# Usage:
#   ./build-web.sh
#
# Output:
#   roguelike/web/   — ready to upload to itch.io (HTML embed)
# ──────────────────────────────────────────────────────────────────

BINARY_NAME="roguelike"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROGUELIKE_DIR="$SCRIPT_DIR/roguelike"
WEB_DIR="$ROGUELIKE_DIR/web"
TARGET_DIR="$SCRIPT_DIR/target/wasm32-unknown-unknown/web-release"

# ── Pre-flight checks ────────────────────────────────────────────

if ! rustup target list --installed | grep -q wasm32-unknown-unknown; then
    echo "⚙  Adding wasm32-unknown-unknown target..."
    rustup target add wasm32-unknown-unknown
fi

if ! command -v wasm-bindgen &> /dev/null; then
    echo "⚙  Installing wasm-bindgen-cli..."
    cargo install wasm-bindgen-cli
fi

# ── Build ─────────────────────────────────────────────────────────

echo "🔨 Building WASM (profile: web-release)..."
cargo build \
    --manifest-path "$ROGUELIKE_DIR/Cargo.toml" \
    --profile web-release \
    --target wasm32-unknown-unknown \
    --no-default-features \
    --features windowed

# ── wasm-bindgen ──────────────────────────────────────────────────

echo "📦 Running wasm-bindgen..."
wasm-bindgen \
    --out-dir "$WEB_DIR" \
    --out-name "$BINARY_NAME" \
    --target web \
    --no-typescript \
    "$TARGET_DIR/$BINARY_NAME.wasm"

# ── Optional wasm-opt ─────────────────────────────────────────────

if command -v wasm-opt &> /dev/null; then
    echo "🗜  Optimizing WASM with wasm-opt..."
    wasm-opt -Oz \
        --output "$WEB_DIR/${BINARY_NAME}_bg.wasm" \
        "$WEB_DIR/${BINARY_NAME}_bg.wasm"
else
    echo "ℹ  wasm-opt not found, skipping optimisation (install with: cargo install wasm-opt)"
fi

# ── Done ──────────────────────────────────────────────────────────

echo ""
echo "✅ Build complete!  Upload the contents of:"
echo "     $WEB_DIR"
echo ""
echo "   to itch.io as an HTML game."
echo ""
echo "   Files:"
ls -lh "$WEB_DIR"
