//! `--scan`: measure files and report on them without writing anything.

use crate::{decode, loudness};
use anyhow::Result;
use std::path::Path;

pub struct Report {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: usize,
    pub bits_per_sample: Option<u32>,
    pub duration_s: f64,
    pub integrated: Option<f64>,
    pub ungated: Option<f64>,
    pub range: Option<f64>,
    pub true_peak: Option<f64>,
    pub noise_floor: Option<f64>,
}

impl Report {
    /// Gap between true peak and integrated loudness (PLR), in dB.
    pub fn peak_to_loudness(&self) -> Option<f64> {
        Some(self.true_peak? - self.integrated?)
    }

    /// The loudest integrated target this file could be normalized to before its peaks
    /// reach 0 dBTP (so the limiter wouldn't have to touch it).
    pub fn max_lufs_for_0dbtp(&self) -> Option<f64> {
        Some(self.integrated? - self.true_peak?)
    }

    /// Indented multi-line summary for the terminal.
    pub fn text(&self) -> String {
        let num = |v: Option<f64>, d: usize| v.map_or("n/a".into(), |v| format!("{v:.d$}"));
        let mut audio = vec![
            pretty_codec(&self.codec),
            format!("{:.1}", self.sample_rate as f64 / 1000.0).trim_end_matches(".0").to_string() + " kHz",
            match self.channels {
                1 => "mono".into(),
                2 => "stereo".into(),
                n => format!("{n} channels"),
            },
        ];
        if let Some(b) = self.bits_per_sample {
            audio.push(format!("{b}-bit"));
        }
        audio.push(duration(self.duration_s));
        format!(
            "    Audio     {}\n    Loudness  {} LUFS integrated (ungated {}), range {} LU\n    Peak      {} dBTP true peak, peak-to-loudness {} dB\n    Headroom  normalize up to {} LUFS before peaks reach 0 dBTP\n    Noise     floor {} dBFS",
            audio.join(" · "),
            num(self.integrated, 2),
            num(self.ungated, 2),
            num(self.range, 1),
            num(self.true_peak, 2),
            num(self.peak_to_loudness(), 1),
            num(self.max_lufs_for_0dbtp(), 1),
            num(self.noise_floor, 1),
        )
    }

    /// One JSON object (one line) for `--json`.
    pub fn json(&self, path: &Path) -> String {
        let num = |v: Option<f64>| match v {
            Some(v) if v.is_finite() => format!("{v:.2}"),
            _ => "null".into(),
        };
        format!(
            "{{\"path\":{},\"codec\":{},\"sample_rate\":{},\"channels\":{},\"bits_per_sample\":{},\"duration_s\":{:.3},\"integrated_lufs\":{},\"ungated_lufs\":{},\"loudness_range_lu\":{},\"true_peak_dbtp\":{},\"peak_to_loudness_db\":{},\"max_lufs_for_0dbtp\":{},\"noise_floor_dbfs\":{}}}",
            json_str(&path.display().to_string()),
            json_str(&self.codec),
            self.sample_rate,
            self.channels,
            self.bits_per_sample.map_or("null".into(), |b| b.to_string()),
            self.duration_s,
            num(self.integrated),
            num(self.ungated),
            num(self.range),
            num(self.true_peak),
            num(self.peak_to_loudness()),
            num(self.max_lufs_for_0dbtp()),
            num(self.noise_floor),
        )
    }
}

/// JSON line for a file that couldn't be scanned.
pub fn json_error(path: &Path, error: &str) -> String {
    format!(
        "{{\"path\":{},\"error\":{}}}",
        json_str(&path.display().to_string()),
        json_str(error)
    )
}

pub fn scan(path: &Path) -> Result<Report> {
    let audio = decode::load(path)?;
    let mut meter = loudness::Meter::new(audio.sample_rate, audio.channels, true);
    meter.push_planar(&audio.samples);
    let frames = audio.samples.first().map_or(0, |c| c.len());
    Ok(Report {
        codec: audio.codec,
        sample_rate: audio.sample_rate,
        channels: audio.channels,
        bits_per_sample: audio.bits_per_sample,
        duration_s: frames as f64 / audio.sample_rate as f64,
        integrated: meter.integrated(true),
        ungated: meter.integrated(false),
        range: loudness::loudness_range(&audio.samples, audio.sample_rate),
        true_peak: meter.true_peak_dbtp(),
        noise_floor: meter.noise_floor_dbfs(),
    })
}

fn pretty_codec(codec: &str) -> String {
    match codec {
        c if c.starts_with("pcm") => "PCM".into(),
        "vorbis" => "Vorbis".into(),
        c => c.to_uppercase(),
    }
}

fn duration(secs: f64) -> String {
    let s = secs.round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out += "\\\"",
            '\\' => out += "\\\\",
            '\n' => out += "\\n",
            '\r' => out += "\\r",
            '\t' => out += "\\t",
            c if (c as u32) < 0x20 => out += &format!("\\u{:04x}", c as u32),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
