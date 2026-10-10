#!/bin/sh
# Run by the .deb/.rpm after install and removal: refresh the application menu and icon caches
# so the launcher and its icon show up (or disappear) without logging out. Never fails the
# package operation; the tools are optional.
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q /usr/share/applications || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true
exit 0
