#!/usr/bin/env bash
# Installs ThinkBoard for the current user (no root needed): the binary into
# ~/.local/bin, a launcher into ~/.local/share/applications, and icons into the
# hicolor icon theme. Run ./install.sh from the extracted release tarball.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"

data="${XDG_DATA_HOME:-$HOME/.local/share}"
bindir="$HOME/.local/bin"
appdir="$data/applications"
icons="$data/icons/hicolor"

mkdir -p "$bindir" "$appdir" "$icons/scalable/apps" "$icons/256x256/apps"

install -m755 "$here/thinkboard" "$bindir/thinkboard"
cp "$here/thinkboard.svg" "$icons/scalable/apps/thinkboard.svg"
cp "$here/thinkboard.png" "$icons/256x256/apps/thinkboard.png"

# Point the launcher's Exec at the installed binary (absolute path, so it works
# whether or not ~/.local/bin is on PATH).
sed "s|^Exec=.*|Exec=$bindir/thinkboard|" "$here/thinkboard.desktop" \
  > "$appdir/thinkboard.desktop"

# Refresh the desktop/icon caches where the tools exist (harmless if they don't).
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$appdir" || true
command -v gtk-update-icon-cache   >/dev/null 2>&1 && gtk-update-icon-cache -f -t "$icons" || true

echo "ThinkBoard installed:"
echo "  binary:   $bindir/thinkboard"
echo "  launcher: $appdir/thinkboard.desktop"
echo "Find it in your app menu, or run 'thinkboard' if ~/.local/bin is on your PATH."
