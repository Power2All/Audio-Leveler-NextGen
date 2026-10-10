//! Audio Leveler NextGen — desktop front-end (`leveler-gui`, built with `--features gui`).
//!
//! It doesn't process audio itself. It turns the settings into a `leveler` command line,
//! runs the CLI, and reads the CLI's output: status lines drive the progress bar, the ✓/✗
//! column and the console view, and `--scan --json` lines fill the loudness columns. So the
//! GUI and the CLI can never disagree about what a setting does.
//!
//! The `leveler` binary is looked up next to `leveler-gui` first, then on `PATH`. Files or
//! folders passed as arguments are added to the list.
//!
//! Settings are remembered between launches (`settings.json` in the OS config folder), and only
//! one window runs at a time: launching again hands any files to the running window, brings it
//! back if minimized, and exits.

#![cfg_attr(windows, windows_subsystem = "windows")] // no console window on Windows

slint::include_modules!();

use slint::{ComponentHandle, Model, ModelRc, SharedString, StandardListViewItem, VecModel};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

/// (shown in the list, `-f` value). Lossless formats come first: see `is_lossy`.
const FORMATS: [(&str, &str); 6] = [
    ("FLAC", "flac"),
    ("WAV", "wav"),
    ("MP3", "mp3"),
    ("OGG Vorbis", "ogg"),
    ("AAC", "aac"),
    ("Opus", "opus"),
];
const BITRATES: [&str; 6] = ["96k", "128k", "160k", "192k", "256k", "320k"];
const BIT_DEPTHS: [(&str, u32); 3] = [("16-bit", 16), ("24-bit", 24), ("32-bit float (WAV)", 32)];
const SAMPLE_RATES: [(&str, u32); 7] = [
    ("Same as source", 0),
    ("44.1 kHz", 44_100),
    ("48 kHz", 48_000),
    ("88.2 kHz", 88_200),
    ("96 kHz", 96_000),
    ("24 kHz", 24_000),
    ("16 kHz", 16_000),
];
/// (shown in the list, LUFS, dBTP). The GUI always passes `-l`/`-p`, so these are just
/// shortcuts that fill those two fields; index 0 means "whatever is in the fields".
const PRESETS: [(&str, f64, f64); 7] = [
    ("Custom", 0.0, 0.0),
    ("Podcast (−16 / −1)", -16.0, -1.0),
    ("ReplayGain (−18 / −1)", -18.0, -1.0),
    ("Streaming (−14 / −1)", -14.0, -1.0),
    ("EBU R128 (−23 / −1)", -23.0, -1.0),
    ("ATSC A/85 (−24 / −2)", -24.0, -2.0),
    ("Very loud (−13 / −1)", -13.0, -1.0),
];
const LEVELER: [(&str, &str); 4] =
    [("Off", "off"), ("Low", "low"), ("Medium", "medium"), ("High", "high")];
const DEHUM: [(&str, u32); 3] = [("Off", 0), ("50 Hz", 50), ("60 Hz", 60)];
const DENOISE: [(&str, &str); 6] = [
    ("Off", "off"),
    ("Auto", "auto"),
    ("Low (6 dB)", "low"),
    ("Medium (12 dB)", "medium"),
    ("High (24 dB)", "high"),
    ("Complete (60 dB)", "complete"),
];
/// Same input extensions the CLI recognises.
const AUDIO_EXTS: [&str; 18] = [
    "flac", "wav", "wave", "mp3", "ogg", "oga", "opus", "aac", "m4a", "mp4", "aif", "aiff",
    "wma", "alac", "mka", "caf", "ac3", "aifc",
];

/// Table cells per row, in the order FileTable in ui/app.slint shows them.
const COL_STATE: usize = 0;
const COL_NAME: usize = 1;
const COL_FORMAT: usize = 2;
const COLUMNS: usize = 8;

fn is_lossy(format_index: i32) -> bool {
    format_index >= 2
}

fn strings(items: impl IntoIterator<Item = &'static str>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(items.into_iter().map(SharedString::from).collect::<Vec<_>>()))
}

fn table_row(cells: Vec<String>) -> ModelRc<StandardListViewItem> {
    ModelRc::new(VecModel::from(
        cells.into_iter().map(|c| StandardListViewItem::from(c.as_str())).collect::<Vec<_>>(),
    ))
}

fn set_cell(row: &ModelRc<StandardListViewItem>, col: usize, text: &str) {
    row.set_row_data(col, StandardListViewItem::from(text));
}

fn has_audio_ext(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// A folder becomes the audio files directly inside it (like the CLI, not recursive), so
/// every table row is one file.
fn expand(path: PathBuf) -> Vec<PathBuf> {
    if !path.is_dir() {
        return vec![path];
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&path)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && has_audio_ext(p))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// The scan columns for one `--scan --json` result.
fn scan_cells(v: &serde_json::Value) -> Vec<(usize, String)> {
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        let short = if err.contains("unsupported") { "unreadable" } else { "error" };
        let mut cells = vec![(COL_FORMAT, short.to_string())];
        cells.extend((COL_FORMAT + 1..COLUMNS).map(|c| (c, String::new())));
        return cells;
    }
    let num = |key: &str| v.get(key).and_then(|x| x.as_f64());
    // Units are in the column headers.
    let fmt = |key: &str| num(key).map_or(String::new(), |x| format!("{x:.1}"));
    let codec = match v.get("codec").and_then(|c| c.as_str()).unwrap_or("?") {
        c if c.starts_with("pcm") => "PCM".to_string(),
        "vorbis" => "Vorbis".to_string(),
        c => c.to_uppercase(),
    };
    let khz = num("sample_rate").map_or(String::new(), |sr| {
        format!("{:.1}", sr / 1000.0).trim_end_matches(".0").to_string() + " kHz"
    });
    let length = num("duration_s").map_or(String::new(), |d| {
        let s = d.round() as u64;
        if s >= 3600 {
            format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
        } else {
            format!("{}:{:02}", s / 60, s % 60)
        }
    });
    vec![
        (COL_FORMAT, format!("{codec} · {khz}")),
        (3, length),
        (4, fmt("integrated_lufs")),
        (5, fmt("true_peak_dbtp")),
        (6, fmt("loudness_range_lu")),
        (7, fmt("max_lufs_for_0dbtp")),
    ]
}

/// What we've learned from the CLI's output so far.
#[derive(Default)]
struct Tracker {
    scan: bool,
    total: Option<usize>,
    done: usize,
    failed: usize,
    summary: Option<String>,
    error: Option<String>,
}

impl Tracker {
    /// Update the counts from one status line; returns the finished file's name and mark.
    fn observe(&mut self, line: &str) -> Option<(String, &'static str)> {
        let t = line.trim_start();
        let name = |rest: &str| rest.split(" — ").next().unwrap_or(rest).to_string();
        if let Some(rest) = t.strip_prefix("Files") {
            // Splash row: "Files    12 → /out" (or "Files    12" when scanning).
            self.total = rest.split_whitespace().next().and_then(|n| n.parse().ok());
        } else if let Some(rest) = line.strip_prefix("[✓] ") {
            self.done += 1;
            return Some((name(rest), "✓"));
        } else if let Some(rest) = line.strip_prefix("[✗] ") {
            self.done += 1;
            self.failed += 1;
            return Some((name(rest), "✗"));
        } else if let Some(rest) = line.strip_prefix("[-] ") {
            self.done += 1;
            return Some((name(rest), "–"));
        } else if t.starts_with("Done:") {
            self.summary = Some(t.to_string());
        } else if t.starts_with("error:") {
            self.error = Some(t.to_string());
        }
        None
    }

    fn progress(&self) -> (f32, bool) {
        match self.total {
            Some(n) if n > 0 => ((self.done as f32 / n as f32).min(1.0), true),
            _ => (0.0, false),
        }
    }

    fn running_status(&self) -> String {
        let verb = if self.scan { "Scanning" } else { "Processing" };
        let mut s = match self.total {
            Some(n) => format!("{verb}… {} of {n} files done", self.done),
            None => format!("{verb}…"),
        };
        if self.failed > 0 {
            s += &format!(", {} failed", self.failed);
        }
        s
    }
}

/// The CLI next to this executable, or `leveler` on PATH.
fn cli_path() -> PathBuf {
    let name = if cfg!(windows) { "leveler.exe" } else { "leveler" };
    std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name(name))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// A number from a text field; an empty field counts as 0.
fn parse_number(field: &str, value: &str) -> Result<f64, String> {
    let v = value.trim().replace(',', ".").replace('−', "-");
    if v.is_empty() {
        return Ok(0.0);
    }
    v.parse::<f64>().map_err(|_| format!("{field}: “{value}” isn't a number."))
}

/// Turn the window's settings into `leveler` arguments.
fn build_args(ui: &AppWindow, inputs: &[String], scan: bool) -> Result<Vec<String>, String> {
    let mut a: Vec<String> = inputs.to_vec();
    if ui.get_jobs() > 0 {
        a.extend(["-j".into(), ui.get_jobs().to_string()]);
    }
    if scan {
        a.extend(["--scan".into(), "--json".into()]);
        return Ok(a);
    }

    a.extend([
        "-o".into(),
        ui.get_output_dir().trim().to_string(),
        "-f".into(),
        FORMATS[ui.get_format_index().max(0) as usize].1.into(),
    ]);
    let lufs = parse_number("Target LUFS", &ui.get_lufs())?;
    let tp = parse_number("Peak dBTP", &ui.get_true_peak())?;
    a.extend(["-l".into(), lufs.to_string(), "-p".into(), tp.to_string()]);
    if !ui.get_normalize() {
        a.push("--no-normalize".into());
    }
    if ui.get_ungated() {
        a.push("--ungated".into());
    }
    a.extend(["-a".into(), LEVELER[ui.get_leveler_index().max(0) as usize].1.into()]);
    let denoise = DENOISE[ui.get_denoise_index().max(0) as usize].1;
    if denoise != "off" {
        a.extend(["--denoise".into(), denoise.into()]);
    }
    let dehum = DEHUM[ui.get_dehum_index().max(0) as usize].1;
    if dehum > 0 {
        a.extend(["--dehum".into(), dehum.to_string()]);
    }
    let highpass = parse_number("High-pass (Hz)", &ui.get_highpass())?;
    if highpass > 0.0 {
        a.extend(["--highpass".into(), highpass.to_string()]);
    }
    let fade = parse_number("Fade (s)", &ui.get_fade())?;
    if fade > 0.0 {
        a.extend(["--fade".into(), fade.to_string()]);
    }

    if is_lossy(ui.get_format_index()) {
        a.extend(["--bitrate".into(), BITRATES[ui.get_bitrate_index().max(0) as usize].into()]);
    } else {
        a.extend([
            "--bit-depth".into(),
            BIT_DEPTHS[ui.get_bit_depth_index().max(0) as usize].1.to_string(),
        ]);
    }
    let sr = SAMPLE_RATES[ui.get_sample_rate_index().max(0) as usize].1;
    if sr > 0 {
        a.extend(["--sample-rate".into(), sr.to_string()]);
    }
    if ui.get_mono() {
        a.push("--mono".into());
    }
    if ui.get_overwrite() {
        a.push("--overwrite".into());
    }
    if ui.get_stats() {
        a.push("--stats".into());
    }
    Ok(a)
}

/// For the console view: the command as you'd type it.
fn shell_line(exe: &Path, args: &[String]) -> String {
    let quote = |s: &str| {
        if s.is_empty() || s.contains(|c: char| c.is_whitespace() || "'\"&()".contains(c)) {
            format!("\"{}\"", s.replace('"', "\\\""))
        } else {
            s.to_string()
        }
    };
    std::iter::once(quote(&exe.display().to_string()))
        .chain(args.iter().map(|a| quote(a)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Forward each line of a child's output stream to the UI.
fn follow(
    stream: impl Read + Send + 'static,
    tracker: Arc<Mutex<Tracker>>,
    ui: slint::Weak<AppWindow>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        while reader.read_until(b'\n', &mut buf).unwrap_or(0) > 0 {
            let line = String::from_utf8_lossy(&buf).trim_end().to_string();
            buf.clear();

            // `--scan --json` results: fill the row's columns, keep them out of the console.
            if line.starts_with('{') {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                    let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
                    let cells = scan_cells(&v);
                    let _ = ui.upgrade_in_event_loop(move |ui| {
                        let i = ui.get_paths().iter().position(|p| p == path.as_str());
                        if let Some(row) = i.and_then(|i| ui.get_files().row_data(i)) {
                            for (col, text) in &cells {
                                set_cell(&row, *col, text);
                            }
                        }
                    });
                    continue;
                }
            }

            let (finished, progress, known, status) = {
                let mut t = tracker.lock().unwrap();
                let finished = t.observe(&line);
                let (p, k) = t.progress();
                (finished, p, k, t.running_status())
            };
            let _ = ui.upgrade_in_event_loop(move |ui| {
                if let Some((name, mark)) = &finished {
                    // Mark the first row with this name that has no mark yet.
                    for row in ui.get_files().iter() {
                        let cell = |c| row.row_data(c).map(|i| i.text).unwrap_or_default();
                        if cell(COL_NAME) == name.as_str() && cell(COL_STATE).is_empty() {
                            set_cell(&row, COL_STATE, mark);
                            break;
                        }
                    }
                }
                ui.set_log(format!("{}{line}\n", ui.get_log()).into());
                ui.set_progress(progress);
                ui.set_progress_known(known);
                ui.set_status(status.into());
                ui.invoke_scroll_log_to_end();
            });
        }
    })
}

/// Per-user folder for the settings file and the single-instance lock.
fn config_dir() -> Option<PathBuf> {
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| home().map(|h| h.join(".config")))
    }?;
    let dir = base.join("audio-leveler-nextgen");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

// ---- Settings ----

fn index_of<T>(items: &[T], matches: impl Fn(&T) -> bool) -> Option<i32> {
    items.iter().position(matches).map(|i| i as i32)
}

/// Save every setting (not the file list). Stored by value rather than list index, so the
/// file stays valid if the choice lists change.
fn save_settings(ui: &AppWindow, dir: &Path) {
    let pick = |i: i32| i.max(0) as usize;
    let json = serde_json::json!({
        "format": FORMATS[pick(ui.get_format_index())].1,
        "bitrate": BITRATES[pick(ui.get_bitrate_index())],
        "bit_depth": BIT_DEPTHS[pick(ui.get_bit_depth_index())].1,
        "sample_rate": SAMPLE_RATES[pick(ui.get_sample_rate_index())].1,
        "mono": ui.get_mono(),
        "preset": PRESETS[pick(ui.get_preset_index())].0,
        "lufs": ui.get_lufs().as_str(),
        "true_peak": ui.get_true_peak().as_str(),
        "normalize": ui.get_normalize(),
        "ungated": ui.get_ungated(),
        "leveler": LEVELER[pick(ui.get_leveler_index())].1,
        "denoise": DENOISE[pick(ui.get_denoise_index())].1,
        "dehum": DEHUM[pick(ui.get_dehum_index())].1,
        "highpass": ui.get_highpass().as_str(),
        "fade": ui.get_fade().as_str(),
        "jobs": ui.get_jobs(),
        "overwrite": ui.get_overwrite(),
        "stats": ui.get_stats(),
        "output_dir": ui.get_output_dir().as_str(),
        "show_console": ui.get_show_log(),
    });
    // Write then rename, so a crash mid-write can't leave a half-written file.
    let tmp = dir.join("settings.json.tmp");
    if let Ok(text) = serde_json::to_string_pretty(&json) {
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join("settings.json"));
        }
    }
}

/// Put every setting back to its default (the same values ui/app.slint starts with). The
/// output folder, the file list and the console toggle are left alone.
fn reset_settings(ui: &AppWindow) {
    ui.set_format_index(0); // FLAC
    ui.set_bitrate_index(4); // 256k
    ui.set_bit_depth_index(0); // 16-bit
    ui.set_sample_rate_index(0); // same as source
    ui.set_mono(false);
    ui.set_preset_index(1); // Podcast
    ui.set_lufs("-16".into());
    ui.set_true_peak("-1".into());
    ui.set_normalize(true);
    ui.set_ungated(false);
    ui.set_leveler_index(2); // Medium
    ui.set_denoise_index(0); // Off
    ui.set_dehum_index(0); // Off
    ui.set_highpass("0".into());
    ui.set_fade("0".into());
    ui.set_jobs(0);
    ui.set_overwrite(false);
    ui.set_stats(false);
    ui.set_lossy_format(is_lossy(ui.get_format_index()));
}

/// Restore saved settings. Missing or unrecognised values keep their defaults.
fn load_settings(ui: &AppWindow, dir: &Path) {
    let Ok(text) = std::fs::read_to_string(dir.join("settings.json")) else { return };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    let s = |k: &str| v.get(k).and_then(|x| x.as_str());
    let n = |k: &str| v.get(k).and_then(|x| x.as_u64());
    let b = |k: &str| v.get(k).and_then(|x| x.as_bool());

    if let Some(i) = s("format").and_then(|f| index_of(&FORMATS, |x| x.1 == f)) {
        ui.set_format_index(i);
    }
    if let Some(i) = s("bitrate").and_then(|f| index_of(&BITRATES, |x| *x == f)) {
        ui.set_bitrate_index(i);
    }
    if let Some(i) = n("bit_depth").and_then(|d| index_of(&BIT_DEPTHS, |x| x.1 as u64 == d)) {
        ui.set_bit_depth_index(i);
    }
    if let Some(i) = n("sample_rate").and_then(|r| index_of(&SAMPLE_RATES, |x| x.1 as u64 == r)) {
        ui.set_sample_rate_index(i);
    }
    if let Some(i) = s("preset").and_then(|p| index_of(&PRESETS, |x| x.0 == p)) {
        ui.set_preset_index(i);
    }
    if let Some(i) = s("leveler").and_then(|l| index_of(&LEVELER, |x| x.1 == l)) {
        ui.set_leveler_index(i);
    }
    if let Some(i) = s("denoise").and_then(|d| index_of(&DENOISE, |x| x.1 == d)) {
        ui.set_denoise_index(i);
    }
    if let Some(i) = n("dehum").and_then(|d| index_of(&DEHUM, |x| x.1 as u64 == d)) {
        ui.set_dehum_index(i);
    }
    if let Some(x) = s("lufs") {
        ui.set_lufs(x.into());
    }
    if let Some(x) = s("true_peak") {
        ui.set_true_peak(x.into());
    }
    if let Some(x) = s("highpass") {
        ui.set_highpass(x.into());
    }
    if let Some(x) = s("fade") {
        ui.set_fade(x.into());
    }
    if let Some(x) = s("output_dir") {
        ui.set_output_dir(x.into());
    }
    if let Some(x) = n("jobs") {
        ui.set_jobs(x.min(256) as i32);
    }
    for (key, set) in [
        ("mono", AppWindow::set_mono as fn(&AppWindow, bool)),
        ("normalize", AppWindow::set_normalize),
        ("ungated", AppWindow::set_ungated),
        ("overwrite", AppWindow::set_overwrite),
        ("stats", AppWindow::set_stats),
        ("show_console", AppWindow::set_show_log),
    ] {
        if let Some(x) = b(key) {
            set(ui, x);
        }
    }
}

// ---- Single instance ----
//
// The first window holds an OS file lock on `gui.lock` for its whole lifetime; the OS drops it
// when the process ends, even after a crash, so a stale lock can't block the next launch. A
// second launch can't take the lock: it appends its file arguments (or an empty line, meaning
// "show yourself") to `inbox`, and the running window picks that up within half a second.

const INBOX: &str = "inbox";

/// Hand our arguments to the running window.
fn send_to_running(dir: &Path, files: &[PathBuf]) {
    use std::io::Write;
    let mut text: String =
        files.iter().map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()).display().to_string() + "\n").collect();
    if text.is_empty() {
        text.push('\n');
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(INBOX)) {
        let _ = f.write_all(text.as_bytes());
    }
}

/// Take whatever other launches have sent us. Renaming first means a launch that appends at
/// the same moment just starts a new inbox instead of being lost.
fn take_inbox(dir: &Path) -> Option<Vec<PathBuf>> {
    let reading = dir.join("inbox.reading");
    std::fs::rename(dir.join(INBOX), &reading).ok()?;
    let text = std::fs::read_to_string(&reading).unwrap_or_default();
    let _ = std::fs::remove_file(&reading);
    Some(text.lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).collect())
}

/// Show the window to the user after another launch: restore it if minimized and ask for
/// focus. Wayland doesn't let an app focus itself, so there the attention request makes the
/// desktop highlight the app instead (KDE: the taskbar entry).
fn bring_forward(ui: &AppWindow) {
    use slint::winit_030::{winit::window::UserAttentionType, WinitWindowAccessor};
    ui.window().set_minimized(false);
    ui.window().with_winit_window(|w| {
        w.set_minimized(false);
        w.focus_window();
        w.request_user_attention(Some(UserAttentionType::Informational));
    });
}

fn main() -> Result<(), slint::PlatformError> {
    let from_args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let dir = config_dir();

    // Keep `_lock` alive until main returns: dropping it releases the lock.
    let _lock = match &dir {
        Some(dir) => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(dir.join("gui.lock"));
            match file {
                Ok(f) if f.try_lock().is_err() => {
                    send_to_running(dir, &from_args);
                    eprintln!(
                        "Audio Leveler NextGen is already running; {}.",
                        if from_args.is_empty() {
                            "bringing it to the front".to_string()
                        } else {
                            format!("added {} item(s) to it", from_args.len())
                        }
                    );
                    return Ok(());
                }
                Ok(f) => {
                    let _ = std::fs::remove_file(dir.join(INBOX)); // leftovers from an old run
                    Some(f)
                }
                Err(_) => None, // can't create the lock file: just run without the guard
            }
        }
        None => None,
    };

    let ui = AppWindow::new()?;
    // On Wayland the desktop finds the window's icon through this ID: it must match the
    // installed `audio-leveler-nextgen.desktop` (see packaging/linux). It needs the platform
    // that `AppWindow::new` sets up, and must come before the window is shown.
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Err(e) = slint::set_xdg_app_id("audio-leveler-nextgen") {
        eprintln!("couldn't set the application id: {e}");
    }
    ui.set_version(
        concat!(env!("CARGO_PKG_VERSION_MAJOR"), ".", env!("CARGO_PKG_VERSION_MINOR")).into(),
    );
    ui.set_format_model(strings(FORMATS.map(|f| f.0)));
    ui.set_bitrate_model(strings(BITRATES));
    ui.set_bit_depth_model(strings(BIT_DEPTHS.map(|b| b.0)));
    ui.set_sample_rate_model(strings(SAMPLE_RATES.map(|s| s.0)));
    ui.set_preset_model(strings(PRESETS.map(|p| p.0)));
    ui.set_leveler_model(strings(LEVELER.map(|l| l.0)));
    ui.set_dehum_model(strings(DEHUM.map(|d| d.0)));
    ui.set_denoise_model(strings(DENOISE.map(|d| d.0)));
    if let Some(dir) = &dir {
        load_settings(&ui, dir);
    }
    ui.set_lossy_format(is_lossy(ui.get_format_index()));

    let rows: Rc<VecModel<ModelRc<StandardListViewItem>>> = Rc::new(VecModel::default());
    let paths: Rc<VecModel<SharedString>> = Rc::new(VecModel::default());
    ui.set_files(rows.clone().into());
    ui.set_paths(paths.clone().into());
    let child: Arc<Mutex<Option<Child>>> = Arc::default();

    let add = {
        let rows = rows.clone();
        let paths = paths.clone();
        let ui = ui.as_weak();
        move |picked: Vec<PathBuf>| {
            for file in picked.into_iter().flat_map(expand) {
                let path = SharedString::from(file.display().to_string());
                if paths.iter().any(|p| p == path) {
                    continue;
                }
                let name =
                    file.file_name().map_or(path.to_string(), |n| n.to_string_lossy().into());
                let mut cells = vec![String::new(); COLUMNS];
                cells[COL_NAME] = name;
                rows.push(table_row(cells));
                paths.push(path);
            }
            if let Some(ui) = ui.upgrade() {
                ui.set_status("Ready. Scan to see loudness, or Start to level.".into());
            }
        }
    };

    // Files or folders given on the command line (`leveler-gui *.flac`, "Open with…").
    if !from_args.is_empty() {
        add(from_args);
    }

    // Pick up files and "show yourself" requests from later launches.
    let inbox_timer = slint::Timer::default();
    if let Some(dir) = dir.clone() {
        let add = add.clone();
        let ui = ui.as_weak();
        inbox_timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(500), move || {
            let Some(files) = take_inbox(&dir) else { return };
            if let Some(ui) = ui.upgrade() {
                bring_forward(&ui);
                // Fine even mid-run: a running job uses its own copy of the file list.
                if !files.is_empty() {
                    add(files);
                }
            }
        });
    }

    ui.on_add_files({
        let add = add.clone();
        move || {
            let picked = rfd::FileDialog::new()
                .set_title("Add audio files")
                .add_filter("Audio files", &AUDIO_EXTS)
                .add_filter("All files", &["*"])
                .pick_files();
            if let Some(files) = picked {
                add(files);
            }
        }
    });

    ui.on_add_folder({
        let add = add.clone();
        move || {
            if let Some(dir) = rfd::FileDialog::new().set_title("Add a folder").pick_folder() {
                add(vec![dir]);
            }
        }
    });

    ui.on_remove_selected({
        let rows = rows.clone();
        let paths = paths.clone();
        let ui = ui.as_weak();
        move || {
            let ui = ui.unwrap();
            let i = ui.get_selected_file();
            if i >= 0 && (i as usize) < rows.row_count() {
                rows.remove(i as usize);
                paths.remove(i as usize);
                ui.set_selected_file(-1);
            }
        }
    });

    ui.on_clear_files({
        let rows = rows.clone();
        let paths = paths.clone();
        let ui = ui.as_weak();
        move || {
            rows.set_vec(Vec::new());
            paths.set_vec(Vec::new());
            let ui = ui.unwrap();
            ui.set_selected_file(-1);
            ui.set_status("List cleared.".into());
        }
    });

    ui.on_browse_output({
        let ui = ui.as_weak();
        move || {
            if let Some(dir) = rfd::FileDialog::new().set_title("Output folder").pick_folder() {
                ui.unwrap().set_output_dir(dir.display().to_string().into());
            }
        }
    });

    ui.on_reset_settings({
        let dir = dir.clone();
        let ui = ui.as_weak();
        move || {
            let ui = ui.unwrap();
            reset_settings(&ui);
            if let Some(dir) = &dir {
                save_settings(&ui, dir);
            }
            ui.set_status("Settings reset to the defaults (output folder kept).".into());
        }
    });

    ui.on_format_chosen({
        let ui = ui.as_weak();
        move |i| ui.unwrap().set_lossy_format(is_lossy(i))
    });

    ui.on_preset_chosen({
        let ui = ui.as_weak();
        move |i| {
            if let Some(&(_, lufs, tp)) = PRESETS.get(i as usize).filter(|_| i > 0) {
                let ui = ui.unwrap();
                ui.set_lufs(lufs.to_string().into());
                ui.set_true_peak(tp.to_string().into());
            }
        }
    });

    ui.on_stop({
        let child = child.clone();
        let ui = ui.as_weak();
        move || {
            if let Some(c) = child.lock().unwrap().as_mut() {
                let _ = c.kill();
                ui.unwrap().set_status("Stopping…".into());
            }
        }
    });

    ui.on_start({
        let dir = dir.clone();
        let rows = rows.clone();
        let paths = paths.clone();
        let child = child.clone();
        let weak = ui.as_weak();
        move |scan| {
            let ui = weak.unwrap();
            if ui.get_running() {
                return;
            }
            let inputs: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
            if inputs.is_empty() {
                ui.set_status("Add some files first.".into());
                return;
            }
            if !scan && ui.get_output_dir().trim().is_empty() {
                ui.set_status("Choose where to save the files first (“Browse…”).".into());
                return;
            }
            let args = match build_args(&ui, &inputs, scan) {
                Ok(a) => a,
                Err(msg) => {
                    ui.set_status(msg.into());
                    return;
                }
            };

            // Fresh marks (and fresh measurements when scanning).
            for row in rows.iter() {
                set_cell(&row, COL_STATE, "");
                if scan {
                    for col in COL_FORMAT..COLUMNS {
                        set_cell(&row, col, "");
                    }
                }
            }

            if let Some(dir) = &dir {
                save_settings(&ui, dir);
            }

            let exe = cli_path();
            ui.set_log(format!("$ {}\n", shell_line(&exe, &args)).into());
            let mut cmd = Command::new(&exe);
            cmd.args(&args)
                .env("NO_COLOR", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            // Windows: the CLI is a console program; without this every run flashes a console
            // window next to the GUI (CREATE_NO_WINDOW).
            #[cfg(windows)]
            std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
            let spawned = cmd.spawn();
            let mut proc = match spawned {
                Ok(p) => p,
                Err(e) => {
                    let msg = format!("Couldn't start {}: {e}", exe.display());
                    ui.set_log(format!("{}{msg}\n", ui.get_log()).into());
                    ui.set_status(msg.into());
                    ui.set_show_log(true);
                    return;
                }
            };

            ui.set_running(true);
            ui.set_progress(0.0);
            ui.set_progress_known(false);
            ui.set_status(if scan { "Scanning…" } else { "Processing…" }.into());

            let tracker = Arc::new(Mutex::new(Tracker { scan, ..Tracker::default() }));
            let readers = [
                follow(proc.stdout.take().unwrap(), tracker.clone(), weak.clone()),
                follow(proc.stderr.take().unwrap(), tracker.clone(), weak.clone()),
            ];
            *child.lock().unwrap() = Some(proc);

            let child = child.clone();
            let weak = weak.clone();
            std::thread::spawn(move || {
                for r in readers {
                    let _ = r.join();
                }
                let exit = child.lock().unwrap().take().map(|mut c| c.wait());
                let t = tracker.lock().unwrap();
                let killed = matches!(&exit, Some(Ok(s)) if s.code().is_none());
                let failed_note =
                    if t.failed > 0 { format!(", {} failed", t.failed) } else { String::new() };
                let status = if killed {
                    format!("Stopped. {} file(s) finished before stopping.", t.done)
                } else if let Some(err) = &t.error {
                    err.clone()
                } else if t.scan {
                    format!("Scanned {} file(s){failed_note}.", t.done)
                } else if let Some(summary) = &t.summary {
                    summary.clone()
                } else {
                    match &exit {
                        Some(Ok(s)) => format!("leveler exited with {s}."),
                        Some(Err(e)) => format!("leveler: {e}"),
                        None => "Finished.".to_string(),
                    }
                };
                // Open the console when something went wrong (for scans, failures already
                // show in the table, so only for a real error there).
                let failed = t.error.is_some() || (!t.scan && t.failed > 0);
                let complete = !killed;
                drop(t);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_running(false);
                    if complete {
                        ui.set_progress(1.0);
                        ui.set_progress_known(true);
                    }
                    if failed {
                        ui.set_show_log(true);
                    }
                    ui.set_status(status.into());
                    ui.invoke_scroll_log_to_end();
                });
            });
        }
    });

    ui.run()?;

    if let Some(dir) = &dir {
        save_settings(&ui, dir);
    }

    // Window closed: don't leave a conversion running in the background.
    if let Some(mut c) = child.lock().unwrap().take() {
        let _ = c.kill();
    }
    Ok(())
}
