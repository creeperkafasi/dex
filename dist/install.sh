#!/bin/sh
# Builds dex and installs it for the current user: binary, app-menu entry, icon.
#   dist/install.sh            install
#   dist/install.sh --remove   uninstall
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=${XDG_BIN_HOME:-$HOME/.local/bin}
DATA=${XDG_DATA_HOME:-$HOME/.local/share}
APPS=$DATA/applications
ICONS=$DATA/icons/hicolor/scalable/apps

if [ "${1:-}" = "--remove" ]; then
    rm -f "$BIN/dex" "$APPS/dex.desktop" "$ICONS/dex.svg"
    echo "removed dex"
    exit 0
fi

cargo build --release --manifest-path "$ROOT/Cargo.toml"
mkdir -p "$BIN" "$APPS" "$ICONS"
install -m 755 "$ROOT/target/release/dex" "$BIN/dex"
install -m 644 "$ROOT/dist/dex.svg" "$ICONS/dex.svg"
# Absolute Exec path: desktop sessions often don't have ~/.local/bin on PATH.
sed "s|^Exec=dex|Exec=$BIN/dex|" "$ROOT/dist/dex.desktop" > "$APPS/dex.desktop"
chmod 644 "$APPS/dex.desktop"

command -v update-desktop-database >/dev/null && update-desktop-database -q "$APPS" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$DATA/icons/hicolor" 2>/dev/null || true
command -v desktop-file-validate >/dev/null && desktop-file-validate "$APPS/dex.desktop" || true
echo "installed: $BIN/dex, $APPS/dex.desktop"
