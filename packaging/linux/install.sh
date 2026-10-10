#!/bin/sh
# Install the desktop launcher and icon for the current user. On Wayland this is what gives the
# window its icon (the desktop matches the window's app ID to the .desktop file); it also adds
# Audio Leveler NextGen to the application menu and to "Open with" for audio files.
#
# Usage: packaging/linux/install.sh [path/to/leveler-gui]   (default: leveler-gui on PATH)
# Not needed with the .deb/.rpm packages, which install the launcher and icon system-wide.
# Remove: rm ~/.local/share/applications/audio-leveler-nextgen.desktop \
#            ~/.local/share/icons/hicolor/scalable/apps/AudioLevelerNextGen.svg
set -e
here=$(cd "$(dirname "$0")" && pwd)
bin=${1:-$(command -v leveler-gui || true)}
if [ -z "$bin" ] || [ ! -x "$bin" ]; then
    echo "leveler-gui not found; pass its path as the first argument" >&2
    exit 1
fi
bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")
data=${XDG_DATA_HOME:-$HOME/.local/share}
mkdir -p "$data/applications" "$data/icons/hicolor/scalable/apps"
icon="$data/icons/hicolor/scalable/apps/AudioLevelerNextGen.svg"
cp "$here/../../ui/icon.svg" "$icon"
# Point Icon= at the file itself: a running Plasma shell keeps its icon-theme index from login
# and won't find a newly installed icon by name until it restarts, so the taskbar would show a
# blank icon until then. A full path is loaded directly.
sed -e "s|^Exec=leveler-gui|Exec=\"$bin\"|" -e "s|^Icon=.*|Icon=$icon|" \
    "$here/audio-leveler-nextgen.desktop" > "$data/applications/audio-leveler-nextgen.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" || true
command -v kbuildsycoca6 >/dev/null && kbuildsycoca6 >/dev/null 2>&1 || true
echo "Installed launcher and icon for $bin"
