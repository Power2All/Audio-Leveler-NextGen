//! Audio decoding via Symphonia (pure Rust) — replaces the ffmpeg decode step.
//! Decodes FLAC, WAV/AIFF, MP3, OGG Vorbis, AAC/M4A, ALAC and more into planar f32.

use anyhow::{anyhow, bail, Context, Result};
use std::fs::File;
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Decoded audio: planar f32 channels at a known sample rate.
pub struct Audio {
    pub sample_rate: u32,
    pub channels: usize,
    pub samples: Vec<Vec<f32>>,
    /// Codec name as Symphonia reports it, e.g. "flac", "mp3", "aac", "pcm_s24le".
    pub codec: String,
    /// Source bit depth, when the format has one (PCM, FLAC, ALAC).
    pub bits_per_sample: Option<u32>,
}

/// Load and fully decode a file to planar f32.
pub fn load(path: &Path) -> Result<Audio> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions { enable_gapless: true, ..Default::default() }, &MetadataOptions::default())
        .with_context(|| format!("unrecognized/unsupported audio format: {}", path.display()))?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow!("no decodable audio track in {}", path.display()))?;
    let track_id = track.id;
    let codec = symphonia::default::get_codecs()
        .get_codec(track.codec_params.codec)
        .map_or("unknown", |d| d.short_name)
        .to_string();
    let bits_per_sample = track.codec_params.bits_per_sample;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("no decoder for this codec")?;

    let mut sample_rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut channels = 0usize;
    let mut out: Vec<Vec<f32>> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // Clean end of stream, or a reset we treat as end.
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(anyhow!("decode error: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymError::DecodeError(_)) => continue, // skip a bad packet
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(anyhow!("decode error: {e}")),
        };

        let spec = *decoded.spec();
        if out.is_empty() {
            channels = spec.channels.count();
            if sample_rate == 0 {
                sample_rate = spec.rate;
            }
            out = vec![Vec::new(); channels];
        }
        let mut sb = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        sb.copy_interleaved_ref(decoded);
        for frame in sb.samples().chunks(channels) {
            for (ch, &s) in frame.iter().enumerate() {
                out[ch].push(s);
            }
        }
    }

    if out.is_empty() || channels == 0 {
        bail!("decoded no audio from {}", path.display());
    }
    if sample_rate == 0 {
        sample_rate = 44_100;
    }
    Ok(Audio { sample_rate, channels, samples: out, codec, bits_per_sample })
}
