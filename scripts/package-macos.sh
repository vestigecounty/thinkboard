#!/bin/bash
# Packages ThinkBoard into a double-clickable macOS .app bundle: launched from
# Finder/Dock it runs with no terminal and shows its icon (Dock, ⌘-Tab, Finder).
# Portable — run it in place or drag to /Applications. Ad-hoc signed (not
# notarized), which is enough to launch locally on Apple Silicon.
#
# `cargo run` is unaffected and stays the dev/terminal path.
set -euo pipefail
cd "$(dirname "$0")/.."

app="target/ThinkBoard.app"
bin="target/release/thinkboard"

echo "Building release binary..."
cargo build --release

echo "Assembling ${app}..."
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/thinkboard"
cp assets/thinkboard.icns "$app/Contents/Resources/thinkboard.icns"

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>ThinkBoard</string>
    <key>CFBundleDisplayName</key><string>ThinkBoard</string>
    <key>CFBundleIdentifier</key><string>com.vestigecounty.thinkboard</string>
    <key>CFBundleExecutable</key><string>thinkboard</string>
    <key>CFBundleIconFile</key><string>thinkboard</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleShortVersionString</key><string>${version}</string>
    <key>CFBundleVersion</key><string>${version}</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Ad-hoc code signature so Gatekeeper on Apple Silicon runs it (an unsigned
# bundle can be killed as "damaged").
if codesign --force --deep --sign - "$app" >/dev/null 2>&1; then
  echo "ad-hoc signed"
else
  echo "codesign unavailable — bundle left unsigned"
fi

echo "Done: $app"
