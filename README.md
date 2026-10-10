# Audio Leveler NextGen

Version 1.0. Two programs: **`leveler`**, the command-line tool, and **`leveler-gui`**, a desktop
app that drives it (see [Desktop app](#desktop-app-leveler-gui)).

A self-contained, cross-platform **batch audio leveler** in Rust, built as an open-source
stand-in for the discontinued **Auphonic Leveler** desktop app. **Nothing is needed at
runtime**: no ffmpeg, no codec libraries. Decoding, processing and encoding all happen inside
the binary, and the static Linux build doesn't even need the system C library.

| Auphonic Leveler feature | How `leveler` does it |
|---|---|
| **Loudness normalization** (LUFS target) | our own ITU-R BS.1770-4 meter + one make-up gain |
| **Measurement gate toggle** (gated / ungated) | our meter (`--ungated`) |
| **True-peak limiter** (dBTP ceiling) | our own look-ahead, 4×-oversampled limiter |
| **Adaptive leveler** (even out loudness over time) | our own sliding-window loudness leveler |
| **Noise/hiss reduction** (+ auto) | our own FFT spectral subtraction, amount can be picked automatically |
| **De-hummer** (50/60 Hz + harmonics) | biquad notch filters |
| **High-pass** (rumble) | biquad high-pass |
| **Mono mixdown** | in-Rust downmix |

**Input** (decoded by Symphonia, pure Rust): FLAC, WAV/AIFF, MP3, OGG Vorbis, AAC/M4A, ALAC and
more. Opus *input* isn't supported, because Symphonia has no Opus decoder yet.

**Output:** FLAC and WAV (pure Rust); MP3 (LAME), OGG Vorbis (libvorbis), AAC (FDK, ADTS) and
Opus (libopus in an OGG container). The C codecs are compiled from source and **linked
statically into the binary**.

> The loudness *measurement and normalization* follow the published BS.1770 standard, so they
> agree with other R128 tools (Auphonic included) to within about 0.1 LU. Process new tracks to
> the same LUFS target you used before and they sit at the same loudness as your existing
> library. The leveler, de-hummer and auto-denoise are our own implementations of the same
> ideas, not ports of Auphonic's code, so the fine detail inside a track won't be bit-identical.

## Building

You need:

- a Rust toolchain;
- for the default (all formats) build: a **C/C++ compiler** (gcc/clang/MSVC) and **CMake**;
- for the fully static Linux build: additionally the **musl C toolchain** (`musl-gcc`, e.g.
  `pacman -S musl`).

- for the desktop app: nothing extra on Windows and macOS; on Linux the usual desktop libraries
  (fontconfig, freetype, xkbcommon, Wayland/X11), which any desktop install already has.

Choose what to build with these aliases (defined in `.cargo/config.toml`):

```bash
cargo build-cli    # only the command-line tool, `leveler`
cargo build-gui    # only the desktop app, `leveler-gui`
cargo build-all    # both
```

They're shorthand for `cargo build --release --bin leveler`, `cargo build --release --features
gui --bin leveler-gui` and `cargo build --release --features gui`. The GUI only builds with the
`gui` feature, so a plain `cargo build --release` gives you just the CLI.

```bash
cargo build --release --target x86_64-unknown-linux-musl     # fully static CLI, all formats
cargo install --path . --target x86_64-unknown-linux-musl --bin leveler   # install it to ~/.cargo/bin
cargo install --path . --features gui --bin leveler-gui                   # install the GUI next to it
```

The GUI runs the `leveler` binary, so install or copy both side by side. It looks next to itself
first, then on `PATH`.

### Where the binaries end up

Normally in `target/release/` (`leveler`, `leveler-gui`) and
`target/x86_64-unknown-linux-musl/release/leveler` (static CLI).

**⚠️ The build path must not contain spaces.** The MP3 encoder builds with autotools/libtool,
which break on spaces. If your checkout path has one (like `…/Auphonic Leveler/leveler-rs`),
build somewhere else: set `CARGO_TARGET_DIR=/some/space-free/folder`, or put a
`.cargo/config.toml` in a folder *above* the checkout, which Cargo also reads and which stays
out of the repository:

```toml
[build]
target-dir = "/home/you/.cache/leveler-build"   # full path; Cargo doesn't expand ~ here
```

The binaries then land in that folder instead of `./target/`.

`.cargo/config.toml` also makes the Opus crate build libopus from its bundled source and link
it statically. Without that, on Linux it would quietly link the system `libopus.so`. These
settings are environment variables the Opus build script doesn't watch, so if you ever change
them, run `cargo clean --release -p audiopus_sys` before rebuilding.

### Build variants

| Variant | Command | Formats | Notes |
|---|---|---|---|
| Default | `cargo build --release` | all six | Needs C/C++ compiler + CMake. On Linux it still uses the system glibc (`libc`, `libm`, `libgcc_s`). |
| Fully static (Linux) | `--target x86_64-unknown-linux-musl` | all six | One self-contained file for any x86-64 Linux. Also needs `musl-gcc`. Runs about 1.3× slower than the glibc build (musl's maths library). |
| Pure Rust | `--no-default-features` | FLAC, WAV | No C toolchain needed for the glibc/Windows/macOS build. The musl variant still needs `musl-gcc`, for the mimalloc allocator the musl build uses. |
| Desktop app | `--features gui` | (uses the CLI) | Built with Slint. Not static: it uses the platform's windowing and font libraries. About 18 MB. |

The static musl build uses **mimalloc** instead of musl's own memory allocator, which is slow
when several threads process files at once. That fix brought the static build from about 1.8×
down to about 1.3× the glibc build's run time.

### Per-platform notes

- **Linux / macOS:** the default build works with gcc/clang + CMake.
- **Windows:** use the MSVC toolchain (Visual Studio Build Tools) plus CMake. On a Windows
  machine the MP3 encoder's build script compiles LAME directly instead of running autotools,
  so no Unix shell is needed. Add `RUSTFLAGS="-C target-feature=+crt-static"` so the `.exe`
  files don't need the Visual C++ runtime installed (the release workflow does). Cross-building
  the all-formats CLI from Linux with MinGW (`x86_64-pc-windows-gnu`) fails in the AAC library;
  the pure-Rust build and the desktop app cross-build fine. The desktop app opens without a
  console window and runs the CLI without one either.
- **AAC licensing:** the FDK AAC library is under Fraunhofer's own licence, not a standard
  open-source one. Check it before redistributing binaries that include AAC.

### Release builds on GitHub

`.github/workflows/release.yml` builds both programs on GitHub and packages them:

| Platform | Package | Contents |
|---|---|---|
| Linux x86-64 | `.deb` and `.rpm` | `leveler` (static) and `leveler-gui` in `/usr/bin`, plus the menu launcher and icon |
| macOS (Apple Silicon + Intel) | `.dmg` | `Audio Leveler NextGen.app` (universal); the CLI is inside it, in `Contents/MacOS/leveler` |
| Windows x86-64 | `.zip` | `leveler.exe`, `leveler-gui.exe` and this README |

Each build levels a test tone into all six output formats as a smoke test. The packages and a
`SHA256SUMS.txt` then go into a **draft** release named after the version in `Cargo.toml`
(`v1.0.0`), for you to check and publish. Run it from the Actions tab ("Run workflow"), or push
the matching tag (a tag that doesn't match `Cargo.toml` stops the run):

```bash
git tag v1.0.0 && git push origin v1.0.0
```

Running it again for the same version replaces the files in the existing release. The release
text comes from `.github/release-notes.md`.

- The Linux packages are built on Ubuntu 22.04, so the desktop app needs glibc 2.35 or newer
  (Ubuntu 22.04, Debian 12, Fedora 36 and later). The CLI is static and runs anywhere.
- The macOS app is signed ad hoc only (no Apple developer account), so macOS blocks the first
  launch: try to open it, then allow it under System Settings → Privacy & Security → "Open
  Anyway", or run `xattr -dr com.apple.quarantine "/Applications/Audio Leveler NextGen.app"`.
  To use the CLI from a terminal, link it onto your `PATH`:
  `sudo ln -s "/Applications/Audio Leveler NextGen.app/Contents/MacOS/leveler" /usr/local/bin/leveler`.
- Packaging files: `packaging/linux/nfpm.yaml` (.deb/.rpm via [nfpm](https://nfpm.goreleaser.com)),
  `packaging/macos/` (`Info.plist`, `AppIcon.icns`, `make-dmg.sh`), `packaging/smoke-test.sh`.

## Usage

```
leveler <INPUT>... -o <DIR> -f <FORMAT> [options]
```

`INPUT` can be repeated and can be a file, a directory (every audio file directly inside it), or
a glob. Quote globs so `leveler` expands them itself. Files are recognised by extension: `flac`,
`wav`, `wave`, `mp3`, `ogg`, `oga`, `aac`, `m4a`, `mp4`, `aif`, `aiff`, `aifc`, `alac`, `caf`,
`mka`, `ac3`, `wma`, `opus` (the last few only if Symphonia can actually decode them).

```bash
# Vinyl rips -> MP3 at podcast loudness, trim rumble, remove 50 Hz hum, 8 files at a time
leveler "/rips/*.flac" -o /out -f mp3 --preset podcast --highpass 20 --dehum 50 -j8

# Whole folder -> 24-bit FLAC at EBU R128, light hiss reduction, stats sidecars
leveler /rips -o /out -f flac --preset ebur128 --bit-depth 24 --denoise low --stats

# Measure files without converting them (see "Scanning files")
leveler --scan "/rips/*.flac"

# Show what would be processed
leveler "/rips/*.flac" -o /out -f mp3 --dry-run
```

Each output keeps the input's base name with the new extension (`.flac`, `.wav`, `.mp3`, `.ogg`,
`.aac`, `.opus`). Existing outputs are skipped unless you pass `--overwrite`.

### What you see while it runs

A short splash on stderr shows the version and the settings for the run, then every file gets
a status line as soon as a worker picks it up:

```
  Audio Leveler NextGen 1.0
  ─────────────────────────
  Files    12 → /out
  Output   MP3 320 kbps
  Loudness -13 LUFS, true peak ≤ -1 dBTP
  Leveler  medium
  Cleanup  high-pass 20 Hz, de-hum 50 Hz
  Workers  12

[✓] side-a-01.flac
[✗] side-a-02.flac — unrecognized/unsupported audio format: …
[-] side-a-03.flac — already exists, skipped
[/] side-a-04.flac
```

`[|]`, `[/]`, `[-]`, `[\]` is a spinner for a file still being worked on. It turns every 250 ms
(the display only redraws then, or when a file starts or finishes, so it costs next to
nothing). Finished files show a green `✓`, a red `✗` with the reason, or a yellow `-` when
skipped. If stderr isn't a terminal, or `NO_COLOR` is set, only the finished lines are printed,
without colours or cursor movement. A `Done: N converted, N skipped, N failed.` summary goes to
stdout at the end, and the exit code is 1 if any file failed.

### Scanning files

`--scan` measures files and reports on them without writing anything; `-o` and `-f` aren't
needed. Files are scanned in parallel with the same status display:

```
[✓] Dr. Peacock & Stormtrooper - A500 Rebooted.mp3
    Audio     MP3 · 48 kHz · stereo · 3:54
    Loudness  -13.00 LUFS integrated (ungated -13.24), range 9.8 LU
    Peak      -2.64 dBTP true peak, peak-to-loudness 10.4 dB
    Headroom  normalize up to -10.4 LUFS before peaks reach 0 dBTP
    Noise     floor -25.4 dBFS
Done: 1 scanned, 0 failed.
```

- **Integrated loudness** is the number the LUFS target refers to; **ungated** is the ATSC-style
  measurement (`--ungated`).
- **Range** is the loudness range (LRA, EBU Tech 3342): how much the loudness varies over the
  track, ignoring the quietest 10% and loudest 5%.
- **Peak-to-loudness** is the gap between true peak and integrated loudness.
- **Headroom** is the loudest `-l` target you could use before the peaks would reach 0 dBTP, i.e.
  before the limiter has to touch the music.
- **Noise** is a rough noise floor, the same figure `--denoise auto` uses.

Add `--json` to get one JSON object per file on stdout instead (the status display stays on
stderr), for spreadsheets or other programs:

```json
{"path":"/rips/a.flac","codec":"flac","sample_rate":44100,"channels":2,"bits_per_sample":24,"duration_s":180.000,"integrated_lufs":-31.06,"ungated_lufs":-31.10,"loudness_range_lu":7.41,"true_peak_dbtp":-18.23,"peak_to_loudness_db":12.83,"max_lufs_for_0dbtp":-12.83,"noise_floor_dbfs":-39.20}
```

A file that can't be read gives `{"path": …, "error": …}`. `--analyze` is an older name for
`--scan` and still works.

### Metadata

Tags are copied from each input to its output and converted to the output format's own tag type
(ID3v2 for MP3, WAV and AAC; Vorbis comments for FLAC, OGG and Opus): title, artist, album, track
and disc numbers, date, genre, comments, cover art and so on. Fields the target tag type can't
hold are dropped. Two kinds of tags are deliberately **not** copied:

- **ReplayGain** values: they describe the old loudness, so players would turn the new file up
  or down wrongly.
- **Encoder** tags: they describe how the source file was made.

If the tags can't be copied (for example, an input format the tag library can't read), the audio
is still written and the status line says `audio OK, tags not copied: …`.

## Desktop app (`leveler-gui`)

A compact window for everything the CLI does. It follows your system's dark or light theme on
Windows, macOS and Linux (on Linux through the desktop portal, so KDE, GNOME and others).

- **File table.** Add files or a folder (a folder adds the audio files directly inside it), remove
  the selected one, or clear the list. Click a row or Tab into the table and use the arrow keys
  to select. You can also pass files on the command line:
  `leveler-gui ~/rips/*.flac`.
- **Scan** fills in each file's format, length, LUFS, true peak, loudness range and "Max LUFS"
  (the headroom figure above) right in the table, without converting anything.
- **Settings** for every CLI option: format with bitrate or bit depth, sample rate, mono, the
  loudness presets (including "Very loud", −13 LUFS) or your own LUFS and peak values,
  normalize and ungated, leveler, denoise, de-hum, high-pass, fade, parallel files, overwrite and
  stats. Hover over a setting's name for a short explanation and, for fields you type into,
  which values are allowed. **Reset settings** (top right) puts every setting back to its
  default; the file list and the output folder stay as they are.
- **Start** converts into the "Save to" folder. A progress bar and status line follow the run,
  each row gets a ✓ or ✗ as it finishes, and **Stop** cancels it.
- **Console** shows the CLI's own output, starting with the exact `leveler` command the GUI ran.
  It opens by itself if a conversion fails.

**Icon and desktop integration.** The app has its own icon (`ui/icon.svg`: meter bars brought
to an even level inside a target band).

- **Windows:** both `.exe` files get the icon at build time (`packaging/windows/icon.ico`, embedded
  by `build.rs`; needs `rc.exe` from MSVC or MinGW's `windres`). The window and taskbar use it.
- **Linux:** the window icon is built in, but on **Wayland** the desktop looks the icon up through
  a launcher file that matches the window's app ID (`audio-leveler-nextgen`). Install it once
  with:

  ```bash
  packaging/linux/install.sh              # uses leveler-gui from PATH, or pass its path
  ```

  That copies the launcher to `~/.local/share/applications/` and the icon to
  `~/.local/share/icons/hicolor/scalable/apps/AudioLevelerNextGen.svg`, which also puts Audio
  Leveler NextGen in your application menu and in "Open with" for audio files. The icon name has
  no dashes on purpose: icon themes trim dash-separated parts when a name is missing, so
  `audio-leveler-nextgen` would turn into the theme's generic audio icon. The installed launcher
  points at the icon by its full path rather than by name, because a running KDE Plasma keeps
  its icon list cached and wouldn't find a newly installed icon until the next login.
- **macOS:** windows don't carry icons there; the Dock icon comes from an `.app` bundle, which
  this project doesn't build yet.

**Remembered settings.** Every setting, including the output folder and whether the console is
open, is saved when you close the window (and when a run starts) and restored at the next launch.
The file list isn't saved. Settings live in `settings.json` in:

- Linux: `$XDG_CONFIG_HOME/audio-leveler-nextgen/` (usually `~/.config/audio-leveler-nextgen/`)
- macOS: `~/Library/Application Support/audio-leveler-nextgen/`
- Windows: `%APPDATA%\audio-leveler-nextgen\`

**Reset settings** (or deleting the file) goes back to the defaults.

**One window at a time.** Launching `leveler-gui` while it's already open doesn't start a second
copy: any files or folders you launched it with are added to the open window, and the window is
brought forward. On Windows, macOS and X11 it's restored if minimized and focused. Wayland
doesn't let an app raise itself, so there the taskbar entry is highlighted instead (verified on
KDE Plasma). The lock is an OS file lock on `gui.lock` in the same folder; the OS releases it
when the app exits, even after a crash, so it can't get stuck.

The GUI doesn't process audio itself: it builds a `leveler` command line from the settings, runs
it, and reads its output. So the GUI and the CLI always behave the same.

## Presets (`--preset`, sets `-l` and `-p`)

| Preset | LUFS | True peak | Use |
|---|---|---|---|
| `podcast` | −16 | −1 dBTP | Podcasts & mobile |
| `replaygain` | −18 | −1 dBTP | ReplayGain similarity |
| `streaming` | −14 | −1 dBTP | Streaming services |
| `ebur128` | −23 | −1 dBTP | EBU R128 (TV, Europe) |
| `atsc` | −24 | −2 dBTP | ATSC A/85 (TV, US) |

`-l` and `-p` override the preset. Without either, the target is −16 LUFS / −1 dBTP. The
original app also offered −13/−15/−19/−20/−26/−27/−31 LUFS and 0/−0.5/−1.5/−3/−4/−5/−6/−9 dBTP;
use `-l` / `-p` for those.

## Options

| Flag | Default | Meaning |
|---|---|---|
| `-o, --output <DIR>` | — | Output directory (created if missing). Not needed with `--scan` |
| `-f, --format <F>` | — | `flac`, `wave` (alias `wav`), `mp3`, `ogg`, `aac`, `opus`. Not needed with `--scan` |
| `--preset <P>` | — | `podcast`, `replaygain`, `streaming`, `ebur128`, `atsc` |
| `-l, --loudness <LUFS>` | −16 | Integrated loudness target |
| `-p, --true-peak <dBTP>` | −1 | Maximum true peak |
| `--ungated` | off | Measure without the BS.1770-2 relative gate (ATSC-style) |
| `-a, --adaptive <S>` | `medium` | Adaptive leveler: `off`, `low`, `medium`, `high` |
| `--highpass <HZ>` | 0 (off) | High-pass cutoff; try `20` for turntable rumble |
| `--dehum <HZ>` | 0 (off) | Mains-hum removal: `50` or `60` |
| `--denoise <A>` | `off` | `off`, `auto`, `low`, `medium`, `high`, `complete` |
| `--mono` | off | Mix down to mono |
| `--fade <SEC>` | 0 (off) | Linear fade in and out |
| `--stats` | off | Write `<output>.stats.json` with before/after loudness and true peak |
| `--scan` | off | Measure and report, don't convert (alias `--analyze`) |
| `--json` | off | With `--scan`: one JSON object per file on stdout |
| `--sample-rate <HZ>` | source rate | Resample the output |
| `--bit-depth <N>` | 16 | WAV: `16`, `24`, or `32` (32-bit float). FLAC: `16` or `24` (32 is written as 24) |
| `--bitrate <R>` | `256k` | MP3, AAC, OGG and Opus bitrate |
| `-j, --jobs <N>` | CPU cores | Files processed at the same time |
| `--no-normalize` | off | Skip the make-up gain to the LUFS target |
| `--overwrite` | off | Replace existing outputs |
| `--dry-run` | off | List what would be processed |

### What the settings do

- **Adaptive leveler strengths** (how hard quiet and loud passages are pulled toward the
  track's average):

  | Strength | Window | Max lift / cut | Pull |
  |---|---|---|---|
  | `low` | 3.0 s | ±9 dB | 65% |
  | `medium` | 2.0 s | ±12 dB | 85% |
  | `high` | 1.3 s | ±15 dB | 100% |

  Moments within 6 dB of the noise floor are treated as silence and never lifted, so the gaps
  between tracks don't get louder.
- **Denoise** amounts: `low` 6 dB, `medium` 12 dB, `high` 24 dB, `complete` 60 dB. `auto` picks
  from the measured noise floor: above −40 dBFS → 24 dB, above −50 → 12 dB, above −60 → 6 dB,
  otherwise none. Start conservative; strong settings can dull the top end.
- **De-hum** notches the mains frequency and up to 8 harmonics, never above 2 kHz.
- **Encoders:** MP3 is constant-bitrate at the nearest standard LAME bitrate, using LAME quality
  level 2 (`lame -h`, the setting the original Auphonic app used). AAC is constant-bitrate,
  AAC-LC in ADTS. OGG is variable-bitrate aiming at `--bitrate`. Opus supports 8, 12, 16, 24
  and 48 kHz (`--sample-rate`); any other rate, including a 44.1 kHz source with no
  `--sample-rate`, is converted to 48 kHz.
- **`--stats`** writes `target_lufs`, `target_true_peak_dbtp`, `measured_before_lufs` (before
  the make-up gain), `measured_after_lufs` and `measured_after_true_peak_dbtp`.

## Processing order

`decode → mono downmix → de-hum → high-pass → denoise → adaptive level → make-up gain to target →
true-peak limit → fade → resample → encode`

## Accuracy and performance

- **Loudness** lands within about 0.1 LU of the target. Validated against ffmpeg's `ebur128` at
  44.1 and 48 kHz. Fades lower the measured value slightly, because they remove energy.
- **Loudness range** (LRA) from `--scan` agrees with ffmpeg's `ebur128` within 0.1 LU.
- **True peak:** the limiter aims 0.3 dB under the ceiling and never lets the gain go above what
  the ceiling requires, so the output stays at or below it.
- **Speed:** one 3-minute stereo track with `-a high --denoise auto` takes about 5 s (FLAC/OGG)
  to 6.5 s (MP3) with the static build, on one core. Batches spread files across cores
  (`-j`); a single file uses one core.
- **Memory:** each file is processed in RAM, a few hundred MB for a long stereo track. Lower
  `-j` for very long files.
