#!/bin/bash
# Regenerates the app icons from a single design — a blue rounded square matching
# the tray icon — into assets/: thinkboard.ico (Windows) and thinkboard.icns
# (macOS). Run this when the design changes; the generated files are committed so
# a normal build/package doesn't need ImageMagick.
#
# Requires ImageMagick (`magick`); the .icns step also needs macOS `iconutil`.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p assets

color="#4a7cff"

# A rounded square at the given pixel size — geometry as fractions of the size
# (margin 3/32, corner radius 7/32, matching src/app/tray.rs), on a transparent
# background — written to stdout as PNG.
draw() {
  local s=$1
  local m=$(( s * 3 / 32 ))
  local r=$(( s * 7 / 32 ))
  local hi=$(( s - m - 1 ))
  magick -size "${s}x${s}" xc:none -fill "$color" \
    -draw "roundrectangle ${m},${m} ${hi},${hi} ${r},${r}" png:-
}

# Windows: one multi-resolution .ico.
draw 256 | magick png:- -define icon:auto-resize=256,128,64,48,32,16 assets/thinkboard.ico

# macOS: an .iconset (each logical size plus its @2x) compiled to .icns.
if command -v iconutil >/dev/null 2>&1; then
  iconset="$(mktemp -d)/thinkboard.iconset"
  mkdir -p "$iconset"
  for entry in 16:16x16 32:16x16@2x 32:32x32 64:32x32@2x \
               128:128x128 256:128x128@2x 256:256x256 512:256x256@2x \
               512:512x512 1024:512x512@2x; do
    draw "${entry%%:*}" > "$iconset/icon_${entry#*:}.png"
  done
  iconutil -c icns "$iconset" -o assets/thinkboard.icns
  rm -rf "$(dirname "$iconset")"
  echo "wrote assets/thinkboard.ico and assets/thinkboard.icns"
else
  echo "wrote assets/thinkboard.ico (iconutil not found; skipped .icns)"
fi
