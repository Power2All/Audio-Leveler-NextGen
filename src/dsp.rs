//! Pure-Rust cleanup DSP that replaces the ffmpeg `-af` chain: high-pass, de-hum,
//! spectral noise reduction, resampling, fades and mono downmix. All operate on planar
//! f32 channels.

use crate::loudness::{band_reject, high_pass};
use anyhow::Result;
use rubato::{FftFixedIn, Resampler};
use rustfft::{num_complex::Complex, FftPlanner};
use std::f64::consts::PI;

/// High-pass every channel at `cutoff` Hz (0 disables).
pub fn highpass(chans: &mut [Vec<f32>], fs: u32, cutoff: f64) {
    if cutoff <= 0.0 {
        return;
    }
    let bq = high_pass(fs as f64, cutoff, 0.707);
    for ch in chans.iter_mut() {
        bq.apply(ch);
    }
}

/// Notch the mains fundamental and up to 8 harmonics (<= 2 kHz).
pub fn dehum(chans: &mut [Vec<f32>], fs: u32, base: u32) {
    if base == 0 {
        return;
    }
    for n in 1..=8u32 {
        let f0 = (base * n) as f64;
        if f0 > 2000.0 || f0 >= fs as f64 / 2.0 {
            break;
        }
        let bq = band_reject(fs as f64, f0, 6.0);
        for ch in chans.iter_mut() {
            bq.apply(ch);
        }
    }
}

/// Mix all channels down to a single averaged channel.
pub fn downmix_mono(chans: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let nch = chans.len();
    if nch <= 1 {
        return chans.to_vec();
    }
    let n = chans[0].len();
    let mut mono = vec![0.0f32; n];
    for i in 0..n {
        let mut s = 0.0f64;
        for ch in chans {
            s += ch[i] as f64;
        }
        mono[i] = (s / nch as f64) as f32;
    }
    vec![mono]
}

/// Linear fade-in and fade-out of `fade_s` seconds.
pub fn fade(chans: &mut [Vec<f32>], fs: u32, fade_s: f64) {
    if fade_s <= 0.0 {
        return;
    }
    let n = chans.first().map(|c| c.len()).unwrap_or(0);
    let fl = ((fs as f64) * fade_s).round() as usize;
    if fl == 0 || n == 0 {
        return;
    }
    let fl = fl.min(n / 2);
    for ch in chans.iter_mut() {
        for i in 0..fl {
            let g = i as f32 / fl as f32;
            ch[i] *= g;
            let j = n - 1 - i;
            ch[j] *= g;
        }
    }
}

/// Resample all channels from `fs_in` to `fs_out`.
pub fn resample(chans: &[Vec<f32>], fs_in: u32, fs_out: u32) -> Result<Vec<Vec<f32>>> {
    if fs_in == fs_out {
        return Ok(chans.to_vec());
    }
    let nch = chans.len();
    let frames = chans.first().map(|c| c.len()).unwrap_or(0);
    let chunk = 1024usize;
    let mut rs = FftFixedIn::<f32>::new(fs_in as usize, fs_out as usize, chunk, 2, nch)?;
    let mut out: Vec<Vec<f32>> = vec![Vec::new(); nch];
    let mut pos = 0usize;
    while pos < frames {
        let need = rs.input_frames_next();
        let mut inbuf: Vec<Vec<f32>> = Vec::with_capacity(nch);
        for ch in chans {
            let end = (pos + need).min(frames);
            let mut v = ch[pos..end].to_vec();
            v.resize(need, 0.0);
            inbuf.push(v);
        }
        let res = rs.process(&inbuf, None)?;
        for ch in 0..nch {
            out[ch].extend_from_slice(&res[ch]);
        }
        pos += need;
    }
    // Trim the padding-induced tail to the theoretical output length.
    let expected = (frames as u64 * fs_out as u64 / fs_in as u64) as usize;
    for ch in out.iter_mut() {
        if ch.len() > expected {
            ch.truncate(expected);
        }
    }
    Ok(out)
}

/// Spectral-subtraction noise reduction. `reduction_db` sets how aggressive: the noise
/// profile is estimated per-bin (minimum statistics) from the whole channel, then
/// over-subtracted with a spectral floor. Our own stand-in for ffmpeg's `afftdn`.
pub fn denoise(chans: &mut [Vec<f32>], reduction_db: f64) {
    if reduction_db <= 0.0 {
        return;
    }
    const N: usize = 1024;
    const HOP: usize = 256;
    let alpha = 1.0 + reduction_db / 10.0; // over-subtraction factor
    let floor = 10f64.powf(-reduction_db / 20.0); // residual spectral floor

    let window: Vec<f32> = (0..N)
        .map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / N as f64).cos()) as f32)
        .collect();

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N);
    let ifft = planner.plan_fft_inverse(N);

    for ch in chans.iter_mut() {
        let n = ch.len();
        if n < N {
            continue;
        }
        // Pass 1: per-bin minimum magnitude across frames (noise estimate).
        let nbins = N / 2 + 1;
        let mut noise = vec![f32::INFINITY; nbins];
        let mut scratch: Vec<Complex<f32>> = vec![Complex::default(); N];
        let mut frame_start = 0;
        while frame_start + N <= n {
            for i in 0..N {
                scratch[i] = Complex::new(ch[frame_start + i] * window[i], 0.0);
            }
            fft.process(&mut scratch);
            for b in 0..nbins {
                let m = scratch[b].norm();
                if m < noise[b] {
                    noise[b] = m;
                }
            }
            frame_start += HOP;
        }
        for b in noise.iter_mut() {
            if !b.is_finite() {
                *b = 0.0;
            }
            *b *= 1.5; // the per-bin minimum underestimates the true noise floor
        }

        // Pass 2: subtract and overlap-add on a copy padded by N zeros at both ends, so
        // every real sample gets the full window overlap. Without the padding the first
        // and last few samples are divided by a near-zero window sum, which turns the
        // spectral smearing into loud clicks at the file edges.
        let mut padded = vec![0.0f32; n + 2 * N];
        padded[N..N + n].copy_from_slice(ch);
        let len = padded.len();
        let mut out = vec![0.0f32; len];
        let mut norm = vec![0.0f32; len];
        frame_start = 0;
        while frame_start + N <= len {
            for i in 0..N {
                scratch[i] = Complex::new(padded[frame_start + i] * window[i], 0.0);
            }
            fft.process(&mut scratch);
            for b in 0..nbins {
                let mag = scratch[b].norm();
                let reduced = (mag - alpha as f32 * noise[b]).max(floor as f32 * mag);
                let scale = if mag > 1e-12 { reduced / mag } else { 0.0 };
                scratch[b] *= scale;
                if b > 0 && b < N - b {
                    scratch[N - b] = scratch[b].conj();
                }
            }
            ifft.process(&mut scratch);
            let inv = 1.0 / N as f32;
            for i in 0..N {
                out[frame_start + i] += scratch[i].re * inv * window[i];
                norm[frame_start + i] += window[i] * window[i];
            }
            frame_start += HOP;
        }
        for i in 0..n {
            ch[i] = out[N + i] / norm[N + i];
        }
    }
}
