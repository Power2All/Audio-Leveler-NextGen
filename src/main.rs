//! Audio Leveler NextGen (`leveler`) — a batch audio leveler in the spirit of the
//! (discontinued) Auphonic Leveler desktop app. Self-contained: no ffmpeg or other tools
//! required at runtime.
//!
//! Pipeline, all in-process:
//!   decode (Symphonia) -> de-hum / high-pass / denoise (our DSP) -> adaptive level
//!   (our leveler) -> measure integrated loudness (our ITU-R BS.1770-4 meter) -> one
//!   make-up gain to the target -> true-peak limit (our limiter) -> fade -> resample
//!   -> encode (FLAC/WAV pure Rust; MP3/OGG/AAC via vendored, statically-linked C).

#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod decode;
mod dsp;
mod encode;
mod leveler;
mod limiter;
mod loudness;
mod scan;
mod tags;
mod ui;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, ValueEnum};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const APP_NAME: &str = "Audio Leveler NextGen";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// File extensions we treat as audio input when expanding a directory or a glob.
const AUDIO_EXTS: &[&str] = &[
    "flac", "wav", "wave", "mp3", "ogg", "oga", "opus", "aac", "m4a", "mp4", "aif", "aiff",
    "wma", "alac", "mka", "caf", "ac3", "aifc",
];

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Format {
    Flac,
    #[value(alias = "wav")]
    Wave,
    Mp3,
    Ogg,
    Aac,
    Opus,
}

impl Format {
    /// File extension and encoder key (they coincide).
    fn ext(self) -> &'static str {
        match self {
            Format::Flac => "flac",
            Format::Wave => "wav",
            Format::Mp3 => "mp3",
            Format::Ogg => "ogg",
            Format::Aac => "aac",
            Format::Opus => "opus",
        }
    }

    /// The matching lofty file type, which decides the tag format the output gets.
    fn file_type(self) -> lofty::file::FileType {
        use lofty::file::FileType;
        match self {
            Format::Flac => FileType::Flac,
            Format::Wave => FileType::Wav,
            Format::Mp3 => FileType::Mpeg,
            Format::Ogg => FileType::Vorbis,
            Format::Aac => FileType::Aac,
            Format::Opus => FileType::Opus,
        }
    }
}

/// Named loudness/true-peak presets, mirroring the targets the Auphonic Leveler offered.
/// Returns (integrated LUFS, max true peak dBTP).
#[derive(Copy, Clone, Debug, ValueEnum)]
enum Preset {
    /// -16 LUFS / -1 dBTP — podcasts and mobile
    Podcast,
    /// -18 LUFS / -1 dBTP — ReplayGain similarity
    Replaygain,
    /// -14 LUFS / -1 dBTP — streaming services
    Streaming,
    /// -23 LUFS / -1 dBTP — EBU R128 (TV, Europe)
    Ebur128,
    /// -24 LUFS / -2 dBTP — ATSC A/85 (TV, US)
    Atsc,
}

impl Preset {
    fn targets(self) -> (f64, f64) {
        match self {
            Preset::Podcast => (-16.0, -1.0),
            Preset::Replaygain => (-18.0, -1.0),
            Preset::Streaming => (-14.0, -1.0),
            Preset::Ebur128 => (-23.0, -1.0),
            Preset::Atsc => (-24.0, -2.0),
        }
    }
}

/// Noise-reduction amount, mirroring the Auphonic denoise levels (6/12/24/100 dB).
/// Returns the reduction in dB to feed ffmpeg's `afftdn`, or `None` when disabled.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Denoise {
    Off,
    Auto,
    Low,
    Medium,
    High,
    Complete,
}

impl Denoise {
    /// Fixed reduction in dB, or `None` when off or decided automatically from the
    /// measured noise floor (see `auto_denoise_db`).
    fn reduction_db(self) -> Option<f64> {
        match self {
            Denoise::Off | Denoise::Auto => None,
            Denoise::Low => Some(6.0),
            Denoise::Medium => Some(12.0),
            Denoise::High => Some(24.0),
            // afftdn caps at ~97 dB; the Auphonic "complete" (100 dB) maps to near-max.
            Denoise::Complete => Some(60.0),
        }
    }
}

/// Map a measured noise floor (dBFS) to a reduction amount, our own stand-in for the
/// original's "Auto" classifier: louder floor -> more reduction, quiet floor -> none.
fn auto_denoise_db(noise_floor_dbfs: f64) -> Option<f64> {
    if noise_floor_dbfs > -40.0 {
        Some(24.0)
    } else if noise_floor_dbfs > -50.0 {
        Some(12.0)
    } else if noise_floor_dbfs > -60.0 {
        Some(6.0)
    } else {
        None
    }
}

/// Strength of our own adaptive leveler stage (see the `leveler` module).
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Strength {
    Off,
    Low,
    Medium,
    High,
}

impl Strength {
    fn params(self) -> Option<leveler::Params> {
        use leveler::Params as P;
        Some(match self {
            Strength::Off => return None,
            Strength::Low => P { window_s: 3.0, max_boost_db: 9.0, max_cut_db: 9.0, pull: 0.65, attack_s: 0.4, release_s: 2.5 },
            Strength::Medium => P { window_s: 2.0, max_boost_db: 12.0, max_cut_db: 12.0, pull: 0.85, attack_s: 0.3, release_s: 1.8 },
            Strength::High => P { window_s: 1.3, max_boost_db: 15.0, max_cut_db: 15.0, pull: 1.0, attack_s: 0.2, release_s: 1.0 },
        })
    }
}

#[derive(Parser, Debug)]
#[command(
    name = APP_NAME,
    bin_name = "leveler",
    version = APP_VERSION,
    about = "Audio Leveler NextGen — batch audio leveler (Auphonic-Leveler-style).",
    long_about = "Own ITU-R BS.1770-4 loudness normalization + adaptive leveler + true-peak \
                  limiter, plus optional de-hum, noise reduction and high-pass, for batches of \
                  FLAC / WAV / MP3 / OGG / AAC / Opus files. Self-contained, no ffmpeg.\n\n\
                  Example:\n  \
                  leveler \"/rips/*.flac\" -o /out -f mp3 --preset podcast --highpass 20 --dehum 50"
)]
struct Cli {
    /// Input files, directories, or glob patterns, e.g. "/rips/*.flac" or /rips
    #[arg(required = true, value_name = "INPUT")]
    inputs: Vec<String>,

    /// Output directory (created if missing). Not needed with --scan.
    #[arg(short = 'o', long = "output", value_name = "DIR", required_unless_present = "scan")]
    output: Option<PathBuf>,

    /// Output format. Not needed with --scan.
    #[arg(short = 'f', long = "format", value_enum, required_unless_present = "scan")]
    format: Option<Format>,

    /// Integrated loudness target in LUFS (default -16; overrides --preset)
    #[arg(short = 'l', long = "loudness", allow_negative_numbers = true)]
    loudness: Option<f64>,

    /// Maximum true peak in dBTP (default -1; overrides --preset)
    #[arg(short = 'p', long = "true-peak", allow_negative_numbers = true)]
    true_peak: Option<f64>,

    /// Loudness/true-peak preset (sets -l and -p unless you pass them explicitly)
    #[arg(long = "preset", value_enum)]
    preset: Option<Preset>,

    /// Disable the BS.1770-2 relative gate (ungated, ATSC-A/85-style measurement)
    #[arg(long = "ungated")]
    ungated: bool,

    /// Scan the inputs and report format, loudness, loudness range, true peak and noise
    /// floor, without converting anything
    #[arg(long = "scan", visible_alias = "analyze")]
    scan: bool,

    /// With --scan: print one JSON object per file on stdout instead of the text report
    #[arg(long = "json", requires = "scan")]
    json: bool,

    /// Adaptive leveler strength
    #[arg(short = 'a', long = "adaptive", value_enum, default_value_t = Strength::Medium)]
    adaptive: Strength,

    /// High-pass filter cutoff in Hz to remove subsonic/turntable rumble (0 = off)
    #[arg(long = "highpass", default_value_t = 0.0)]
    highpass: f64,

    /// Hum (mains) removal: notch out this fundamental + harmonics, e.g. 50 or 60 (0 = off)
    #[arg(long = "dehum", default_value_t = 0)]
    dehum: u32,

    /// Broadband noise/hiss reduction amount
    #[arg(long = "denoise", value_enum, default_value_t = Denoise::Off)]
    denoise: Denoise,

    /// Mix down to mono
    #[arg(long = "mono")]
    mono: bool,

    /// Fade in and out by this many seconds (0 = off)
    #[arg(long = "fade", default_value_t = 0.0)]
    fade: f64,

    /// Write a <output>.stats.json sidecar with measured loudness per file
    #[arg(long = "stats")]
    stats: bool,

    /// Resample output to this sample rate in Hz (default: keep the source rate)
    #[arg(long = "sample-rate", value_name = "HZ")]
    sample_rate: Option<u32>,

    /// Bit depth for FLAC/WAV output: 16 (default), 24 or 32 (WAV: 32-bit float; FLAC: max 24)
    #[arg(long = "bit-depth", value_name = "BITS")]
    bit_depth: Option<u32>,

    /// Bitrate for MP3 / AAC / OGG output, e.g. 256k or 320k
    #[arg(long = "bitrate", default_value = "256k")]
    bitrate: String,

    /// Number of files to process in parallel (0 = one per CPU core)
    #[arg(short = 'j', long = "jobs", default_value_t = 0)]
    jobs: usize,

    /// Skip the loudness normalization pass (adaptive leveling only)
    #[arg(long = "no-normalize")]
    no_normalize: bool,

    /// Overwrite output files that already exist
    #[arg(long = "overwrite")]
    overwrite: bool,

    /// List what would be processed without converting anything
    #[arg(long = "dry-run")]
    dry_run: bool,
}

impl Cli {
    /// Output format; clap guarantees it's set unless scanning.
    fn fmt(&self) -> Format {
        self.format.expect("--format is required when converting")
    }

    /// Output directory; clap guarantees it's set unless scanning.
    fn out(&self) -> &Path {
        self.output.as_deref().expect("--output is required when converting")
    }

    /// Resolve the integrated-loudness and true-peak targets from an explicit value,
    /// then a preset, then the built-in default.
    fn targets(&self) -> (f64, f64) {
        let (pl, pp) = self.preset.map(Preset::targets).unwrap_or((-16.0, -1.0));
        (self.loudness.unwrap_or(pl), self.true_peak.unwrap_or(pp))
    }

    /// Output sample rate: explicit `--sample-rate`, else the source rate. Opus only
    /// accepts 8/12/16/24/48 kHz, so snap it to 48 kHz when needed.
    fn out_sr(&self, native: u32) -> u32 {
        let base = self.sample_rate.unwrap_or(native);
        if matches!(self.format, Some(Format::Opus))
            && ![8_000, 12_000, 16_000, 24_000, 48_000].contains(&base)
        {
            48_000
        } else {
            base
        }
    }

    /// Parse `--bitrate` (e.g. "256k", "192") into kbps.
    fn bitrate_kbps(&self) -> u32 {
        let s = self.bitrate.trim().trim_end_matches(['k', 'K']);
        s.parse::<u32>().unwrap_or(256)
    }
}

/// One planned unit of work.
#[derive(Debug, Clone)]
struct Job {
    input: PathBuf,
    output: PathBuf,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    if let Some(bd) = cli.bit_depth {
        if !matches!(bd, 16 | 24 | 32) {
            bail!("--bit-depth must be 16, 24 or 32 (got {bd})");
        }
    }

    let jobs = collect_jobs(&cli).context("collecting input files")?;
    if jobs.is_empty() {
        bail!("no matching audio files found for the given input(s)");
    }

    if cli.dry_run {
        ui::splash(&format!("{APP_NAME} {APP_VERSION}"), &[("Files", jobs.len().to_string())]);
        println!("Would process {} file(s) -> {}:", jobs.len(), cli.out().display());
        for j in &jobs {
            println!("  {}  ->  {}", j.input.display(), j.output.display());
        }
        return Ok(());
    }

    if !cli.scan {
        std::fs::create_dir_all(cli.out())
            .with_context(|| format!("creating output directory {:?}", cli.out()))?;
    }

    let workers = match cli.jobs {
        0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
        n => n,
    }
    .min(jobs.len());

    let rows = if cli.scan {
        vec![
            ("Files", jobs.len().to_string()),
            ("Mode", "scan only, nothing is written".to_string()),
            ("Workers", workers.to_string()),
        ]
    } else {
        splash_rows(&cli, jobs.len(), workers)
    };
    ui::splash(&format!("{APP_NAME} {APP_VERSION}"), &rows);

    let next = AtomicUsize::new(0);
    let failures = AtomicUsize::new(0);
    let skipped = AtomicUsize::new(0);
    let progress = ui::Progress::new();

    std::thread::scope(|s| {
        s.spawn(|| progress.run());
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = jobs.get(i) else { break };
                    let label = job.input.file_name().map_or_else(
                        || job.input.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    progress.start(i, label);
                    let result = if cli.scan {
                        scan_one(&cli, job)
                    } else {
                        match process_one(&cli, job) {
                            Ok(Outcome::Done(note)) => ui::Finish::Done(note),
                            Ok(Outcome::Skipped) => ui::Finish::Skipped,
                            Err(e) => ui::Finish::Failed(format!("{e:#}")),
                        }
                    };
                    match result {
                        ui::Finish::Skipped => {
                            skipped.fetch_add(1, Ordering::Relaxed);
                        }
                        ui::Finish::Failed(_) => {
                            failures.fetch_add(1, Ordering::Relaxed);
                        }
                        _ => {}
                    }
                    progress.finish(i, result);
                })
            })
            .collect();
        for h in handles {
            let _ = h.join();
        }
        progress.stop();
    });

    let failed = failures.load(Ordering::Relaxed);
    let skip = skipped.load(Ordering::Relaxed);
    let ok = jobs.len() - failed - skip;
    if cli.scan {
        // With --json, stdout carries only the JSON lines.
        if !cli.json {
            println!("Done: {ok} scanned, {failed} failed.");
        }
    } else {
        println!("Done: {ok} converted, {skip} skipped, {failed} failed.");
    }

    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

enum Outcome {
    /// Converted; optional note for the status line.
    Done(Option<String>),
    Skipped,
}

/// Expand the user's inputs (files, directories, globs) into concrete jobs.
fn collect_jobs(cli: &Cli) -> Result<Vec<Job>> {
    let mut inputs: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for pat in &cli.inputs {
        let p = Path::new(pat);
        if p.is_dir() {
            for entry in std::fs::read_dir(p).with_context(|| format!("reading dir {pat}"))? {
                let path = entry?.path();
                if path.is_file() && has_audio_ext(&path) {
                    push_unique(&mut inputs, &mut seen, path);
                }
            }
        } else if p.is_file() {
            // Before the glob check: names like "Side A [Remastered].flac" are real files,
            // not patterns ("[Remastered]" would match a single letter and find nothing).
            push_unique(&mut inputs, &mut seen, p.to_path_buf());
        } else if is_glob(pat) {
            for m in glob::glob(pat).with_context(|| format!("bad glob pattern {pat}"))? {
                let path = m?;
                if path.is_file() && has_audio_ext(&path) {
                    push_unique(&mut inputs, &mut seen, path);
                }
            }
        } else {
            bail!("input not found: {pat}");
        }
    }

    if cli.scan {
        return Ok(inputs.into_iter().map(|input| Job { output: input.clone(), input }).collect());
    }

    let out_ext = cli.fmt().ext();
    let mut jobs = Vec::with_capacity(inputs.len());
    for input in inputs {
        let stem = input
            .file_stem()
            .ok_or_else(|| anyhow!("cannot derive a name from {:?}", input))?;
        let mut out_name = stem.to_os_string();
        out_name.push(".");
        out_name.push(out_ext);
        let output = cli.out().join(out_name);

        // Never let an output clobber its own input.
        if same_file(&input, &output) {
            bail!(
                "output would overwrite input ({}); choose a different output dir or format",
                input.display()
            );
        }
        jobs.push(Job { input, output });
    }
    Ok(jobs)
}

fn push_unique(v: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, p: PathBuf) {
    let key = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
    if seen.insert(key) {
        v.push(p);
    }
}

fn is_glob(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[')
}

fn has_audio_ext(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Scan one file: a report under its status line, or a JSON line on stdout with --json.
fn scan_one(cli: &Cli, job: &Job) -> ui::Finish {
    let result = scan::scan(&job.input);
    if cli.json {
        let line = match &result {
            Ok(r) => r.json(&job.input),
            Err(e) => scan::json_error(&job.input, &format!("{e:#}")),
        };
        println!("{line}");
    }
    match result {
        Ok(_) if cli.json => ui::Finish::Done(None),
        Ok(r) => ui::Finish::Report(r.text()),
        Err(e) => ui::Finish::Failed(format!("{e:#}")),
    }
}

/// Process a single file end to end, entirely in-process (no ffmpeg).
fn process_one(cli: &Cli, job: &Job) -> Result<Outcome> {
    if job.output.exists() && !cli.overwrite {
        return Ok(Outcome::Skipped);
    }

    let (target_lufs, ceiling) = cli.targets();
    let do_normalize = !cli.no_normalize;
    let level_params = cli.adaptive.params();

    // Decode (Symphonia) -> planar f32 at the source rate.
    let audio = decode::load(&job.input)?;
    let fs = audio.sample_rate;
    let mut chans = audio.samples;

    // Mono downmix first, so everything downstream operates on the final channel layout.
    if cli.mono {
        chans = dsp::downmix_mono(&chans);
    }

    // Cleanup filters.
    dsp::dehum(&mut chans, fs, cli.dehum);
    dsp::highpass(&mut chans, fs, cli.highpass);
    let denoise_db = match cli.denoise {
        Denoise::Off => None,
        Denoise::Auto => measure(&chans, fs, cli.ungated, false).2.and_then(auto_denoise_db),
        other => other.reduction_db(),
    };
    if let Some(db) = denoise_db {
        dsp::denoise(&mut chans, db);
    }

    // Our adaptive leveler, pulling each moment toward the file's own integrated loudness.
    if let Some(params) = level_params {
        let (reference, _tp, nf) = measure(&chans, fs, cli.ungated, false);
        if let Some(reference) = reference {
            leveler::apply(&mut chans, fs, reference, nf.unwrap_or(-70.0), &params);
        }
    }

    // One constant make-up gain so the integrated loudness lands exactly on target.
    let mut pre_gain_lufs = None;
    if do_normalize {
        if let (Some(cur), _, _) = measure(&chans, fs, cli.ungated, false) {
            pre_gain_lufs = Some(cur);
            let g = 10f64.powf((target_lufs - cur) / 20.0) as f32;
            for ch in chans.iter_mut() {
                for s in ch.iter_mut() {
                    *s *= g;
                }
            }
        }
    }

    // True-peak ceiling (make-up gain or leveling can lift peaks).
    if do_normalize || level_params.is_some() {
        limiter::apply(&mut chans, fs, ceiling);
    }

    dsp::fade(&mut chans, fs, cli.fade);

    // Resample to the requested output rate.
    let out_sr = cli.out_sr(fs);
    if out_sr != fs {
        chans = dsp::resample(&chans, fs, out_sr)?;
    }

    if cli.stats {
        let (out_i, out_tp, _) = measure(&chans, out_sr, cli.ungated, true);
        write_stats(job, target_lufs, ceiling, pre_gain_lufs, out_i, out_tp)?;
    }

    let opts = encode::Opts {
        sample_rate: out_sr,
        bitrate_kbps: cli.bitrate_kbps(),
        bit_depth: cli.bit_depth,
    };
    encode::write(&job.output, cli.fmt().ext(), &chans, &opts)
        .with_context(|| format!("encoding {}", job.output.display()))?;

    // The audio is written; a tag problem shouldn't fail the file, just say so.
    let note = tags::copy(&job.input, &job.output, cli.fmt().file_type())
        .err()
        .map(|e| format!("audio OK, tags not copied: {e:#}"));
    Ok(Outcome::Done(note))
}

/// The settings summary shown under the splash title.
fn splash_rows(cli: &Cli, files: usize, workers: usize) -> Vec<(&'static str, String)> {
    let mut output = match cli.fmt() {
        Format::Flac | Format::Wave => format!(
            "{} {}-bit",
            if matches!(cli.fmt(), Format::Flac) { "FLAC" } else { "WAV" },
            match cli.bit_depth.unwrap_or(16) {
                32 if matches!(cli.fmt(), Format::Flac) => 24,
                b => b,
            }
        ),
        f => format!("{} {} kbps", f.ext().to_uppercase(), cli.bitrate_kbps()),
    };
    if let Some(sr) = cli.sample_rate {
        output += &format!(", {sr} Hz");
    }
    if cli.mono {
        output += ", mono";
    }

    let (lufs, tp) = cli.targets();
    let loudness = if cli.no_normalize {
        "not normalized".to_string()
    } else {
        format!(
            "{lufs} LUFS, true peak ≤ {tp} dBTP{}",
            if cli.ungated { " (ungated)" } else { "" }
        )
    };

    let mut cleanup = Vec::new();
    if cli.highpass > 0.0 {
        cleanup.push(format!("high-pass {} Hz", cli.highpass));
    }
    if cli.dehum > 0 {
        cleanup.push(format!("de-hum {} Hz", cli.dehum));
    }
    if cli.denoise != Denoise::Off {
        cleanup.push(format!("denoise {:?}", cli.denoise).to_lowercase());
    }
    if cli.fade > 0.0 {
        cleanup.push(format!("fade {} s", cli.fade));
    }

    let mut rows = vec![
        ("Files", format!("{files} → {}", cli.out().display())),
        ("Output", output),
        ("Loudness", loudness),
        ("Leveler", format!("{:?}", cli.adaptive).to_lowercase()),
    ];
    if !cleanup.is_empty() {
        rows.push(("Cleanup", cleanup.join(", ")));
    }
    rows.push(("Workers", workers.to_string()));
    rows
}

/// Measure integrated loudness, true peak (only if `true_peak`) and noise floor of planar
/// PCM in one pass.
fn measure(
    chans: &[Vec<f32>],
    fs: u32,
    ungated: bool,
    true_peak: bool,
) -> (Option<f64>, Option<f64>, Option<f64>) {
    let mut m = loudness::Meter::new(fs, chans.len(), true_peak);
    m.push_planar(chans);
    (m.integrated(!ungated), m.true_peak_dbtp(), m.noise_floor_dbfs())
}

/// Write a sidecar JSON with the chosen targets and the before/after measurements.
fn write_stats(
    job: &Job,
    target_lufs: f64,
    ceiling: f64,
    before_lufs: Option<f64>,
    after_lufs: Option<f64>,
    after_tp: Option<f64>,
) -> Result<()> {
    let path = job.output.with_extension("stats.json");
    let fmt = |o: Option<f64>| o.map(|v| format!("{v:.2}")).unwrap_or("null".into());
    let body = format!(
        "{{\n  \"input\": {:?},\n  \"output\": {:?},\n  \"target_lufs\": {target_lufs},\n  \"target_true_peak_dbtp\": {ceiling},\n  \"measured_before_lufs\": {},\n  \"measured_after_lufs\": {},\n  \"measured_after_true_peak_dbtp\": {}\n}}\n",
        job.input.to_string_lossy(),
        job.output.to_string_lossy(),
        fmt(before_lufs),
        fmt(after_lufs),
        fmt(after_tp),
    );
    std::fs::write(&path, body).with_context(|| format!("writing stats {path:?}"))
}

