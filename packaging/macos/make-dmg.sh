#!/bin/sh
# Wrap leveler-gui and leveler in "Audio Leveler NextGen.app" and put that in a .dmg with an
# Applications shortcut to drag it onto. macOS only (codesign, hdiutil).
#
# Usage: packaging/macos/make-dmg.sh <version> <leveler> <leveler-gui> <out.dmg>
#
# The CLI goes next to the GUI in Contents/MacOS, where the GUI looks for it first. No Apple
# developer account is needed: the bundle is signed ad hoc ("-"), which Apple Silicon requires
# to run anything at all. Downloads still get Gatekeeper's "unidentified developer" warning.
set -e
version=$1 cli=$2 gui=$3 out=$4
here=$(cd "$(dirname "$0")" && pwd)
app="Audio Leveler NextGen.app"
stage=$(mktemp -d)

mkdir -p "$stage/$app/Contents/MacOS" "$stage/$app/Contents/Resources"
cp "$gui" "$stage/$app/Contents/MacOS/leveler-gui"
cp "$cli" "$stage/$app/Contents/MacOS/leveler"
chmod 755 "$stage/$app/Contents/MacOS/"*
sed "s/@VERSION@/$version/g" "$here/Info.plist" > "$stage/$app/Contents/Info.plist"
cp "$here/AppIcon.icns" "$stage/$app/Contents/Resources/AppIcon.icns"

# Sign the helper first, then the bundle (which signs its main executable).
codesign --force --sign - "$stage/$app/Contents/MacOS/leveler"
codesign --force --sign - "$stage/$app"
codesign --verify --strict "$stage/$app"

ln -s /Applications "$stage/Applications"
rm -f "$out"
hdiutil create -volname "Audio Leveler NextGen" -srcfolder "$stage" -fs HFS+ -format UDZO "$out"
rm -rf "$stage"
echo "Created $out"
