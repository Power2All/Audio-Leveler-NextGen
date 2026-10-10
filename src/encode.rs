//! Encoding without ffmpeg. FLAC and WAV are pure Rust; MP3 (LAME), OGG Vorbis, AAC
//! (FDK, ADTS) and Opus (libopus, in an OGG container) use vendored C libraries statically
//! compiled into the binary.

use anyhow::{bail, Context, Result};
#[cfg(feature = "lossy")]
use std::num::{NonZeroU32, NonZeroU8};
use std::path::Path;

pub struct Opts {
    pub sample_rate: u32,
    /// Only used by the lossy encoders.
    #[cfg_attr(not(feature = "lossy"), allow(dead_code))]
    pub bitrate_kbps: u32,
    pub bit_depth: Option<u32>,
}

pub fn write(path: &Path, fmt: &str, chans: &[Vec<f32>], o: &Opts) -> Result<()> {
    match fmt {
        "wav" => wav(path, chans, o),
        "flac" => flac(path, chans, o),
        #[cfg(feature = "lossy")]
        "mp3" => mp3(path, chans, o),
        #[cfg(feature = "lossy")]
        "ogg" => ogg(path, chans, o),
        #[cfg(feature = "lossy")]
        "aac" => aac(path, chans, o),
        #[cfg(feature = "lossy")]
        "opus" => opus(path, chans, o),
        #[cfg(not(feature = "lossy"))]
        "mp3" | "ogg" | "aac" | "opus" => {
            bail!("'{fmt}' output needs the 'lossy' build feature (this binary was built with --no-default-features); use flac or wav, or rebuild with lossy enabled")
        }
        other => bail!("unsupported output format: {other}"),
    }
}

fn interleave(chans: &[Vec<f32>]) -> Vec<f32> {
    let nch = chans.len();
    let n = chans.first().map(|c| c.len()).unwrap_or(0);
    let mut out = Vec::with_capacity(n * nch);
    for i in 0..n {
        for ch in chans {
            out.push(ch[i]);
        }
    }
    out
}

#[cfg(feature = "lossy")]
fn to_i16(chans: &[Vec<f32>]) -> Vec<i16> {
    interleave(chans)
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16)
        .collect()
}

// ---- WAV (hound) ----
fn wav(path: &Path, chans: &[Vec<f32>], o: &Opts) -> Result<()> {
    use hound::{SampleFormat, WavSpec, WavWriter};
    let nch = chans.len();
    let n = chans.first().map(|c| c.len()).unwrap_or(0);
    let bits = o.bit_depth.unwrap_or(16);
    let (bits_per_sample, sample_format) = match bits {
        32 => (32, SampleFormat::Float),
        24 => (24, SampleFormat::Int),
        _ => (16, SampleFormat::Int),
    };
    let spec = WavSpec {
        channels: nch as u16,
        sample_rate: o.sample_rate,
        bits_per_sample,
        sample_format,
    };
    let mut w = WavWriter::create(path, spec).with_context(|| format!("creating {}", path.display()))?;
    for i in 0..n {
        for ch in chans {
            let s = ch[i];
            match (bits_per_sample, sample_format) {
                (32, SampleFormat::Float) => w.write_sample(s)?,
                (24, _) => w.write_sample((s.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32)?,
                _ => w.write_sample((s.clamp(-1.0, 1.0) * 32767.0).round() as i16)?,
            }
        }
    }
    w.finalize().context("finalizing WAV")?;
    Ok(())
}

// ---- FLAC (flacenc, pure Rust) ----
fn flac(path: &Path, chans: &[Vec<f32>], o: &Opts) -> Result<()> {
    use flacenc::bitsink::ByteSink;
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;
    use flacenc::source::MemSource;

    let nch = chans.len();
    let bits = o.bit_depth.unwrap_or(16).clamp(16, 24);
    let peak = ((1i64 << (bits - 1)) - 1) as f32;
    let samples: Vec<i32> = interleave(chans)
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * peak).round() as i32)
        .collect();

    let mut config = flacenc::config::Encoder::default();
    config.block_size = 4096;
    config.block_size = config.block_size.max(256);
    let config = config.into_verified().map_err(|e| anyhow::anyhow!("flac config: {e:?}"))?;
    let source = MemSource::from_samples(&samples, nch, bits as usize, o.sample_rate as usize);
    let mut stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| anyhow::anyhow!("flac encode: {e:?}"))?;
    // 8 KiB PADDING block, as the reference encoder writes, so tags can be added in place.
    // It also matters for correctness: lofty (tags.rs) mishandles a FLAC whose STREAMINFO
    // is the only metadata block, leaving the "last block" flag on STREAMINFO so players
    // never see the tags it adds.
    let padding = flacenc::component::MetadataBlockData::new_unknown(1, &[0u8; 8192])
        .map_err(|e| anyhow::anyhow!("flac padding: {e:?}"))?;
    stream.add_metadata_block(padding);
    let mut sink = ByteSink::new();
    stream.write(&mut sink).map_err(|e| anyhow::anyhow!("flac write: {e:?}"))?;
    std::fs::write(path, sink.as_slice()).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

// ---- MP3 (LAME) ----
#[cfg(feature = "lossy")]
fn mp3(path: &Path, chans: &[Vec<f32>], o: &Opts) -> Result<()> {
    use mp3lame_encoder::{Builder, FlushNoGap, InterleavedPcm};
    let nch = chans.len();
    let mut b = Builder::new().context("LAME builder")?;
    b.set_num_channels(nch as u8).ok();
    b.set_sample_rate(o.sample_rate).ok();
    b.set_brate(nearest_mp3_bitrate(o.bitrate_kbps)).ok();
    // LAME -q 2 (`lame -h`, what the original Auphonic Leveler used): ~2x faster than -q 0.
    b.set_quality(mp3lame_encoder::Quality::NearBest).ok();
    let mut enc = b.build().map_err(|e| anyhow::anyhow!("LAME init: {e:?}"))?;

    let pcm = interleave(chans);
    let mut out: Vec<u8> = Vec::new();
    out.reserve(mp3lame_encoder::max_required_buffer_size(pcm.len() / nch));
    let n = enc
        .encode(InterleavedPcm(&pcm), out.spare_capacity_mut())
        .map_err(|e| anyhow::anyhow!("mp3 encode: {e:?}"))?;
    unsafe { out.set_len(out.len() + n) };
    out.reserve(8192);
    let n = enc
        .flush::<FlushNoGap>(out.spare_capacity_mut())
        .map_err(|e| anyhow::anyhow!("mp3 flush: {e:?}"))?;
    unsafe { out.set_len(out.len() + n) };
    std::fs::write(path, &out).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(feature = "lossy")]
fn nearest_mp3_bitrate(kbps: u32) -> mp3lame_encoder::Bitrate {
    use mp3lame_encoder::Bitrate::*;
    let table = [
        (8, Kbps8), (16, Kbps16), (24, Kbps24), (32, Kbps32), (40, Kbps40), (48, Kbps48),
        (64, Kbps64), (80, Kbps80), (96, Kbps96), (112, Kbps112), (128, Kbps128),
        (160, Kbps160), (192, Kbps192), (224, Kbps224), (256, Kbps256), (320, Kbps320),
    ];
    table
        .iter()
        .min_by_key(|(k, _)| (*k as i64 - kbps as i64).abs())
        .map(|(_, b)| *b)
        .unwrap_or(mp3lame_encoder::Bitrate::Kbps256)
}

// ---- OGG Vorbis (libvorbis) ----
#[cfg(feature = "lossy")]
fn ogg(path: &Path, chans: &[Vec<f32>], o: &Opts) -> Result<()> {
    use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder};
    let nch = chans.len();
    let sr = NonZeroU32::new(o.sample_rate).context("sample rate")?;
    let ch = NonZeroU8::new(nch as u8).context("channels")?;
    let file = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut builder = VorbisEncoderBuilder::new(sr, ch, file)?;
    let target = NonZeroU32::new((o.bitrate_kbps.max(32)) * 1000).unwrap();
    builder.bitrate_management_strategy(VorbisBitrateManagementStrategy::Vbr { target_bitrate: target });
    let mut enc = builder.build()?;
    // Feed libvorbis in modest planar blocks: one whole-track block is pathologically slow
    // (a 3-minute track took ~17 s that way vs ~1.2 s in 4096-sample blocks).
    let n = chans.first().map_or(0, |c| c.len());
    for start in (0..n).step_by(4096) {
        let end = (start + 4096).min(n);
        let block: Vec<&[f32]> = chans.iter().map(|c| &c[start..end]).collect();
        enc.encode_audio_block(&block)?;
    }
    enc.finish()?;
    Ok(())
}

// ---- AAC (FDK, ADTS) ----
#[cfg(feature = "lossy")]
fn aac(path: &Path, chans: &[Vec<f32>], o: &Opts) -> Result<()> {
    use fdk_aac::enc::{AudioObjectType, BitRate, ChannelMode, Encoder, EncoderParams, Transport};
    let nch = chans.len();
    let enc = Encoder::new(EncoderParams {
        bit_rate: BitRate::Cbr(o.bitrate_kbps * 1000),
        sample_rate: o.sample_rate,
        transport: Transport::Adts,
        channels: if nch >= 2 { ChannelMode::Stereo } else { ChannelMode::Mono },
        audio_object_type: AudioObjectType::Mpeg4LowComplexity,
    })
    .map_err(|e| anyhow::anyhow!("aac init: {e:?}"))?;

    let input = to_i16(chans);
    let mut out: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 16_384];
    let mut pos = 0usize;
    // Feed until consumed, then drain with empty input.
    loop {
        let slice = if pos < input.len() { &input[pos..] } else { &[][..] };
        let info = enc
            .encode(slice, &mut buf)
            .map_err(|e| anyhow::anyhow!("aac encode: {e:?}"))?;
        if info.output_size > 0 {
            out.extend_from_slice(&buf[..info.output_size]);
        }
        if pos < input.len() {
            if info.input_consumed == 0 && info.output_size == 0 {
                break;
            }
            pos += info.input_consumed;
        } else if info.output_size == 0 {
            break;
        }
    }
    std::fs::write(path, &out).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

// ---- Opus (libopus, OGG container) ----
#[cfg(feature = "lossy")]
fn opus(path: &Path, chans: &[Vec<f32>], o: &Opts) -> Result<()> {
    use audiopus::{coder::Encoder, Application, Bitrate, Channels, SampleRate};
    use ogg::{PacketWriteEndInfo, PacketWriter};

    let nch = chans.len();
    // libopus takes 8/12/16/24/48 kHz; the pipeline snaps anything else to 48 kHz.
    let rate = SampleRate::try_from(o.sample_rate as i32)
        .map_err(|_| anyhow::anyhow!("opus needs 8/12/16/24/48 kHz, got {}", o.sample_rate))?;
    // Ogg Opus counts granule positions and pre-skip in 48 kHz samples at every input rate.
    let to_48k = 48_000 / o.sample_rate as u64;
    let channels = if nch >= 2 { Channels::Stereo } else { Channels::Mono };
    let mut enc = Encoder::new(rate, channels, Application::Audio)
        .map_err(|e| anyhow::anyhow!("opus init: {e}"))?;
    enc.set_bitrate(Bitrate::BitsPerSecond((o.bitrate_kbps * 1000) as i32)).ok();
    // The lookahead is reported at the encoder's rate.
    let pre_skip = enc.lookahead().map_or(312, |l| l as u64 * to_48k);

    let input = to_i16(chans); // interleaved i16
    let frame = o.sample_rate as usize / 50; // 20 ms per channel
    let fstep = frame * nch;

    let file = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut pw = PacketWriter::new(file);
    let serial: u32 = 0xA0A0_1234;

    // OpusHead (RFC 7845), alone on the first (BOS) page.
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // version
    head.push(nch as u8);
    head.extend_from_slice(&(pre_skip as u16).to_le_bytes());
    head.extend_from_slice(&o.sample_rate.to_le_bytes()); // original input rate (informational)
    head.extend_from_slice(&0i16.to_le_bytes()); // output gain
    head.push(0); // channel mapping family 0
    pw.write_packet(head, serial, PacketWriteEndInfo::EndPage, 0)?;

    // OpusTags, its own page.
    let vendor = b"leveler";
    let mut tags = Vec::new();
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes()); // user comment count
    pw.write_packet(tags, serial, PacketWriteEndInfo::EndPage, 0)?;

    let total = input.len();
    let nframes = total.div_ceil(fstep).max(1);
    let mut outbuf = vec![0u8; 4000];
    let mut granule: u64 = 0;
    for fi in 0..nframes {
        let start = fi * fstep;
        let end = (start + fstep).min(total);
        let mut buf = if start < total { input[start..end].to_vec() } else { Vec::new() };
        buf.resize(fstep, 0);
        let n = enc
            .encode(&buf, &mut outbuf)
            .map_err(|e| anyhow::anyhow!("opus encode: {e}"))?;
        granule += frame as u64 * to_48k;
        let inf = if fi + 1 == nframes {
            PacketWriteEndInfo::EndStream
        } else {
            PacketWriteEndInfo::NormalPacket
        };
        // Page granule = total 48 kHz samples decoded, including the pre-skip.
        pw.write_packet(outbuf[..n].to_vec(), serial, inf, granule + pre_skip)?;
    }
    Ok(())
}
