Every package contains both the desktop app (`leveler-gui`) and the command-line tool (`leveler`).

- **Windows:** unzip and run `leveler-gui.exe`. Keep both `.exe` files in the same folder.
- **Debian / Ubuntu:** `sudo apt install ./audio-leveler-nextgen_*_amd64.deb`
- **Fedora / openSUSE / RHEL:** `sudo dnf install ./audio-leveler-nextgen-*.x86_64.rpm`
- **macOS** (Apple Silicon and Intel): open the `.dmg` and drag the app to Applications. The app
  isn't signed with an Apple developer ID, so macOS blocks the first launch: try to open it once,
  then allow it under System Settings → Privacy & Security → "Open Anyway". Or run
  `xattr -dr com.apple.quarantine "/Applications/Audio Leveler NextGen.app"`.
  The command-line tool is inside the app:
  `/Applications/Audio Leveler NextGen.app/Contents/MacOS/leveler`.

The AAC encoder (FDK AAC) is under Fraunhofer's own licence; check it before redistributing.
