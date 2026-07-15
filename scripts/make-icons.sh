#!/bin/bash
# Regenerates the app + tray icons from the raster master
# (assets/thinkboard_1024.png, itself rendered from the editable
# assets/thinkboard.svg) into assets/:
#   thinkboard.ico        Windows app icon (embedded in the .exe by build.rs)
#   thinkboard.icns       macOS app icon (bundled by scripts/package-macos.sh)
#   thinkboard.png        256px app / window icon (Linux window + taskbar)
#   thinkboard_tray.png   64px tray / menu-bar icon (embedded by src/app/tray.rs)
# Run this when the icon changes; the generated files are committed so a normal
# build/package needs no image tools.
#
# Requires ImageMagick (`magick`); the .icns step also needs macOS `iconutil`.
set -euo pipefail
cd "$(dirname "$0")/.."

src="assets/thinkboard_1024.png"

# Windows: one multi-resolution .ico.
magick "$src" -define icon:auto-resize=256,128,64,48,32,16 assets/thinkboard.ico

# Linux/window icon (256px) and the small tray icon (64px), forced to true RGBA.
magick "$src" -resize 256x256 PNG32:assets/thinkboard.png
magick "$src" -resize 64x64 PNG32:assets/thinkboard_tray.png

# macOS: an .iconset (each logical size plus its @2x) compiled to .icns.
if command -v iconutil >/dev/null 2>&1; then
  iconset="$(mktemp -d)/thinkboard.iconset"
  mkdir -p "$iconset"
  for entry in 16:16x16 32:16x16@2x 32:32x32 64:32x32@2x \
               128:128x128 256:128x128@2x 256:256x256 512:256x256@2x \
               512:512x512 1024:512x512@2x; do
    magick "$src" -resize "${entry%%:*}x${entry%%:*}" "$iconset/icon_${entry#*:}.png"
  done
  iconutil -c icns "$iconset" -o assets/thinkboard.icns
  rm -rf "$(dirname "$iconset")"
  echo "wrote assets/thinkboard.{ico,icns,png} and assets/thinkboard_tray.png"
else
  echo "wrote assets/thinkboard.{ico,png} + tray (iconutil not found; skipped .icns)"
fi
