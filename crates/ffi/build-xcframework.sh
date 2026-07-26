#!/usr/bin/env bash
#
# Build ThinkBoardFFI.xcframework + the generated Swift bindings for iOS.
#
# Output (under crates/ffi/build/):
#   ThinkBoardFFI.xcframework   device (arm64) + simulator (arm64) static libs
#   ThinkBoardFFI.swift         the generated Swift API — add to your app target
#
# Requirements:
#   rustup target add aarch64-apple-ios aarch64-apple-ios-sim
#   Xcode (for xcodebuild -create-xcframework)
#
# Usage:  crates/ffi/build-xcframework.sh [debug|release]   (default: release)
set -euo pipefail

PROFILE="${1:-release}"
CRATE=thinkboard-ffi
LIB=libthinkboard_ffi.a
DEVICE=aarch64-apple-ios
SIM=aarch64-apple-ios-sim

# Minimum iOS the Rust libraries are built for. Without this, cargo defaults
# the minimum to the SDK version (e.g. 18.4), which would silently raise the
# app's real floor above its deployment target. Keep in sync with
# apps/ios/project.yml's deploymentTarget.
export IPHONEOS_DEPLOYMENT_TARGET=16.0

# Repo root (this script lives in crates/ffi/).
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

BUILD="crates/ffi/build"
GEN="$BUILD/gen"
HEADERS="$BUILD/headers"
rm -rf "$BUILD"
mkdir -p "$GEN" "$HEADERS"

# cargo's release flag is `--release`; debug takes no flag. The output dir is
# `release` or `debug` accordingly.
CARGO_FLAG=""
[ "$PROFILE" = "release" ] && CARGO_FLAG="--release"

echo "▸ Generating Swift bindings (from a host build)…"
cargo build -q -p "$CRATE"
cargo run -q -p "$CRATE" --bin uniffi-bindgen -- \
  generate --library "target/debug/libthinkboard_ffi.dylib" \
  --language swift --out-dir "$GEN"

echo "▸ Building iOS static libs ($PROFILE): device + simulator…"
cargo build -q -p "$CRATE" $CARGO_FLAG --target "$DEVICE"
cargo build -q -p "$CRATE" $CARGO_FLAG --target "$SIM"

# The xcframework needs the C header + a `module.modulemap` (uniffi emits the
# same content under its own name) so Swift can import the low-level module.
cp "$GEN/thinkboard_ffiFFI.h" "$HEADERS/"
cp "$GEN/thinkboard_ffiFFI.modulemap" "$HEADERS/module.modulemap"

echo "▸ Assembling xcframework…"
xcodebuild -create-xcframework \
  -library "target/$DEVICE/$PROFILE/$LIB" -headers "$HEADERS" \
  -library "target/$SIM/$PROFILE/$LIB" -headers "$HEADERS" \
  -output "$BUILD/ThinkBoardFFI.xcframework"

cp "$GEN/thinkboard_ffi.swift" "$BUILD/ThinkBoardFFI.swift"

echo
echo "✓ Done."
echo "    $BUILD/ThinkBoardFFI.xcframework   (add to your Xcode project)"
echo "    $BUILD/ThinkBoardFFI.swift         (add to your app target)"
