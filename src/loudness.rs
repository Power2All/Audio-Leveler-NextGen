//! ITU-R BS.1770-4 / EBU R128 loudness measurement, implemented from the published
//! standard (not derived from any third-party source).
//!
//! The standard defines:
//!   * a two-stage "K-weighting" pre-filter (a high-shelf + a high-pass),
//!   * mean-square energy over 400 ms blocks with 75 % overlap (100 ms step),
//!   * a two-stage gate: an absolute gate at -70 LUFS, then a relative gate 10 LU below
//!     the mean loudness of the blocks that passed the absolute gate.
//!
//! This module streams samples through that chain so a whole file never has to live in
//! RAM. It also estimates the true peak (4x-oversampled) and a crude noise floor, both of
//! which the pipeline uses.

use std::collections::VecDeque;
use std::f64::consts::PI;

/// A normalized biquad (a0 == 1). Shared with the cleanup filters in `dsp`.
#[derive(Clone, Copy)]
pub(crate) struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

#[derive(Clone, Copy, Default)]
struct BqState {
    z1: f64,
    z2: f64,
}

impl Biquad {
    #[inline]
    fn process(&self, st: &mut BqState, x: f64) -> f64 {
        // Transposed direct form II.
        let y = self.b0 * x + st.z1;
        st.z1 = self.b1 * x - self.a1 * y + st.z2;
        st.z2 = self.b2 * x - self.a2 * y;
        y
    }

    /// Filter one channel in place.
    pub(crate) fn apply(&self, ch: &mut [f32]) {
        let mut st = BqState::default();
        for s in ch.iter_mut() {
            *s = self.process(&mut st, *s as f64) as f32;
        }
    }
}

/// RBJ-cookbook high-shelf, used for the K-weighting stage-1 pre-filter.
fn high_shelf(fs: f64, f0: f64, q: f64, gain_db: f64) -> Biquad {
    let a = 10f64.powf(gain_db / 40.0);
    let w0 = 2.0 * PI * f0 / fs;
    let (sw, cw) = w0.sin_cos();
    let alpha = sw / (2.0 * q);
    let sa = a.sqrt();
    let b0 = a * ((a + 1.0) + (a - 1.0) * cw + 2.0 * sa * alpha);
    let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cw);
    let b2 = a * ((a + 1.0) + (a - 1.0) * cw - 2.0 * sa * alpha);
    let a0 = (a + 1.0) - (a - 1.0) * cw + 2.0 * sa * alpha;
    let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cw);
    let a2 = (a + 1.0) - (a - 1.0) * cw - 2.0 * sa * alpha;
    Biquad { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
}

/// RBJ-cookbook band-reject (notch) `bw_hz` wide, used by the de-hummer.
pub(crate) fn band_reject(fs: f64, f0: f64, bw_hz: f64) -> Biquad {
    let w0 = 2.0 * PI * f0 / fs;
    let (sw, cw) = w0.sin_cos();
    let alpha = sw / (2.0 * (f0 / bw_hz));
    let a0 = 1.0 + alpha;
    Biquad { b0: 1.0 / a0, b1: -2.0 * cw / a0, b2: 1.0 / a0, a1: -2.0 * cw / a0, a2: (1.0 - alpha) / a0 }
}

/// RBJ-cookbook high-pass: K-weighting stage-2 ("RLB") filter and the rumble filter.
pub(crate) fn high_pass(fs: f64, f0: f64, q: f64) -> Biquad {
    let w0 = 2.0 * PI * f0 / fs;
    let (sw, cw) = w0.sin_cos();
    let alpha = sw / (2.0 * q);
    let b0 = (1.0 + cw) / 2.0;
    let b1 = -(1.0 + cw);
    let b2 = (1.0 + cw) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cw;
    let a2 = 1.0 - alpha;
    Biquad { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
}

/// The two K-weighting stages for a given sample rate.
///
/// At 48 kHz we use the exact coefficients tabulated in BS.1770-4 so our integrated
/// loudness matches libebur128/Auphonic to the bit. The pipeline always measures at
/// 48 kHz (loudness is a level, independent of the analysis rate), so this is the path
/// that runs; the RBJ-cookbook fallback for other rates is kept only for completeness.
fn k_weighting(fs: f64) -> (Biquad, Biquad) {
    if (fs - 48000.0).abs() < 1.0 {
        (
            Biquad {
                b0: 1.53512485958697,
                b1: -2.69169618940638,
                b2: 1.19839281085285,
                a1: -1.69065929318241,
                a2: 0.73248077421585,
            },
            Biquad {
                b0: 1.0,
                b1: -2.0,
                b2: 1.0,
                a1: -1.99004745483398,
                a2: 0.99007225036621,
            },
        )
    } else {
        (
            high_shelf(fs, 1681.974450955533, 0.7071752369554196, 3.999843853973347),
            high_pass(fs, 38.13547087602444, 0.5003270373238773),
        )
    }
}

/// BS.1770 channel weighting. L/R/C are 1.0; surround channels get +1.5 dB (×1.41).
/// For mono/stereo (the vinyl case) everything is 1.0.
fn channel_gain(nch: usize, ch: usize) -> f64 {
    if nch <= 2 {
        1.0
    } else if ch < 3 {
        1.0
    } else {
        1.41
    }
}

/// -0.691 is the absolute calibration offset from the standard (the "LKFS" constant).
const OFFSET: f64 = -0.691;

/// Streaming loudness/true-peak/noise-floor meter.
pub struct Meter {
    nch: usize,
    bs: usize,   // 400 ms block, in samples
    step: usize, // 100 ms step, in samples
    c1: Biquad,
    c2: Biquad,
    s1: Vec<BqState>,
    s2: Vec<BqState>,
    win: Vec<VecDeque<f64>>, // last `bs` K-weighted squared samples per channel
    run_sum: Vec<f64>,       // running sum of the window per channel
    processed: usize,        // samples per channel seen so far
    block_powers: Vec<f64>,  // Σ_ch G_ch * mean-square, one per gating block

    // True peak (4x polyphase oversampling). Optional: it costs ~3x the rest of the meter.
    track_true_peak: bool,
    fir: [Vec<f64>; 4],
    tp_hist: Vec<Vec<f64>>,
    true_peak: f64,

    // Crude noise floor: non-overlapping 100 ms unweighted mean-square blocks.
    nf_bs: usize,
    nf_run: f64,
    nf_count: usize,
    nf_blocks: Vec<f64>,
}

impl Meter {
    /// `track_true_peak` enables the (expensive) 4x-oversampled true-peak estimate.
    pub fn new(fs: u32, nch: usize, track_true_peak: bool) -> Self {
        let fsf = fs as f64;
        let (c1, c2) = k_weighting(fsf);
        let nf_bs = (fsf * 0.1).round() as usize;
        Meter {
            nch,
            bs: (fsf * 0.4).round() as usize,
            step: (fsf * 0.1).round() as usize,
            c1,
            c2,
            s1: vec![BqState::default(); nch],
            s2: vec![BqState::default(); nch],
            win: vec![VecDeque::new(); nch],
            run_sum: vec![0.0; nch],
            processed: 0,
            block_powers: Vec::new(),
            track_true_peak,
            fir: design_oversample_fir(TP_TAPS),
            tp_hist: vec![vec![0.0; TP_TAPS]; nch],
            true_peak: 0.0,
            nf_bs: nf_bs.max(1),
            nf_run: 0.0,
            nf_count: 0,
            nf_blocks: Vec::new(),
        }
    }

    /// Feed deinterleaved (planar) channels, as the DSP pipeline holds them.
    pub fn push_planar(&mut self, channels: &[Vec<f32>]) {
        let n = channels.first().map(|c| c.len()).unwrap_or(0);
        let nch = channels.len();
        let mut frame = vec![0.0f32; nch];
        for i in 0..n {
            for ch in 0..nch {
                frame[ch] = channels[ch][i];
            }
            self.push(&frame);
        }
    }

    /// Feed interleaved samples (length must be a multiple of the channel count).
    pub fn push(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks_exact(self.nch) {
            let mut nf_ms = 0.0;
            for ch in 0..self.nch {
                let x = frame[ch] as f64;

                // True-peak: slide history, evaluate the 4 interpolated sub-samples.
                if self.track_true_peak {
                    let hist = &mut self.tp_hist[ch];
                    hist.rotate_right(1);
                    hist[0] = x;
                    self.true_peak = self.true_peak.max(oversampled_peak(&self.fir, hist));
                }

                // K-weighting -> squared -> sliding window.
                let y = self.c2.process(&mut self.s2[ch], self.c1.process(&mut self.s1[ch], x));
                let sq = y * y;
                let dq = &mut self.win[ch];
                dq.push_back(sq);
                self.run_sum[ch] += sq;
                if dq.len() > self.bs {
                    self.run_sum[ch] -= dq.pop_front().unwrap();
                }

                nf_ms += x * x;
            }

            // Noise-floor block (unweighted, averaged across channels).
            self.nf_run += nf_ms / self.nch as f64;
            self.nf_count += 1;
            if self.nf_count >= self.nf_bs {
                self.nf_blocks.push(self.nf_run / self.nf_count as f64);
                self.nf_run = 0.0;
                self.nf_count = 0;
            }

            self.processed += 1;
            // Emit a gating block at 400 ms, then every 100 ms.
            if self.processed >= self.bs && (self.processed - self.bs) % self.step == 0 {
                let mut p = 0.0;
                for ch in 0..self.nch {
                    p += channel_gain(self.nch, ch) * (self.run_sum[ch] / self.bs as f64);
                }
                self.block_powers.push(p);
            }
        }
    }

    /// Integrated loudness in LUFS, or `None` if the signal was too short/silent.
    /// With `gated == false` only the absolute -70 LUFS gate is applied (the ungated,
    /// ATSC-A/85-style measurement); with `true`, the BS.1770-2 relative gate is added.
    pub fn integrated(&self, gated: bool) -> Option<f64> {
        let abs_thresh = power_from_loudness(-70.0);
        let abs: Vec<f64> = self
            .block_powers
            .iter()
            .copied()
            .filter(|&p| p > abs_thresh && p > 0.0)
            .collect();
        if abs.is_empty() {
            return None;
        }
        let kept: Vec<f64> = if gated {
            let mean_abs = abs.iter().sum::<f64>() / abs.len() as f64;
            let rel = power_from_loudness(loudness_from_power(mean_abs) - 10.0);
            abs.into_iter().filter(|&p| p > rel).collect()
        } else {
            abs
        };
        if kept.is_empty() {
            return None;
        }
        Some(loudness_from_power(kept.iter().sum::<f64>() / kept.len() as f64))
    }

    /// Estimated true peak in dBTP, or `None` if the meter wasn't tracking it.
    pub fn true_peak_dbtp(&self) -> Option<f64> {
        self.track_true_peak.then(|| 20.0 * self.true_peak.max(1e-12).log10())
    }

    /// Crude noise floor in dBFS: the 10th-percentile 100 ms RMS block. Useful as an
    /// "is there hiss?" signal for auto noise reduction, not a precise measurement.
    pub fn noise_floor_dbfs(&self) -> Option<f64> {
        if self.nf_blocks.is_empty() {
            return None;
        }
        let mut v = self.nf_blocks.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let idx = (v.len() as f64 * 0.10) as usize;
        let ms = v[idx.min(v.len() - 1)];
        Some(10.0 * ms.max(1e-20).log10())
    }
}

pub fn loudness_from_power(p: f64) -> f64 {
    OFFSET + 10.0 * p.log10()
}
fn power_from_loudness(l: f64) -> f64 {
    10f64.powf((l - OFFSET) / 10.0)
}

/// Per-hop short-term loudness (LUFS) over a centered window, for the adaptive leveler.
/// `win`/`hop` are in samples. This is the control signal the leveler acts on.
pub fn short_term_loudness(channels: &[Vec<f32>], fs: u32, win: usize, hop: usize) -> Vec<f64> {
    let (c1, c2) = k_weighting(fs as f64);
    let nch = channels.len();
    let n = channels.first().map(|c| c.len()).unwrap_or(0);
    // K-weighted squared signal per channel.
    let mut fsq: Vec<Vec<f32>> = Vec::with_capacity(nch);
    for ch in channels {
        let mut s1 = BqState::default();
        let mut s2 = BqState::default();
        let mut v = Vec::with_capacity(ch.len());
        for &x in ch {
            let y = c2.process(&mut s2, c1.process(&mut s1, x as f64));
            v.push((y * y) as f32);
        }
        fsq.push(v);
    }
    let half = win / 2;
    let (mut lo, mut hi) = (0usize, 0usize);
    let mut rs = vec![0.0f64; nch];
    let mut out = Vec::new();
    let mut center = 0usize;
    while center < n {
        let nlo = center.saturating_sub(half);
        let nhi = (center + half).min(n);
        while hi < nhi {
            for ch in 0..nch {
                rs[ch] += fsq[ch][hi] as f64;
            }
            hi += 1;
        }
        while lo < nlo {
            for ch in 0..nch {
                rs[ch] -= fsq[ch][lo] as f64;
            }
            lo += 1;
        }
        let len = (hi - lo).max(1) as f64;
        let mut p = 0.0;
        for ch in 0..nch {
            p += channel_gain(nch, ch) * (rs[ch] / len);
        }
        out.push(loudness_from_power(p.max(1e-20)));
        center += hop;
    }
    out
}

/// Loudness range (LRA) in LU, as defined by EBU Tech 3342: the spread between the 10th and
/// 95th percentile of 3-second short-term loudness, after an absolute gate at -70 LUFS and a
/// relative gate 20 LU below the mean of the remaining readings. `None` for audio shorter
/// than 3 s or silence.
pub fn loudness_range(channels: &[Vec<f32>], fs: u32) -> Option<f64> {
    let n = channels.first()?.len();
    let win = 3 * fs as usize;
    let hop = (fs as usize / 10).max(1); // 10 readings per second
    if n < win {
        return None;
    }
    let half = win / 2;
    // short_term_loudness centres its windows; keep only readings with a full 3 s window.
    let gated: Vec<f64> = short_term_loudness(channels, fs, win, hop)
        .into_iter()
        .enumerate()
        .filter(|&(k, l)| k * hop >= half && k * hop + half <= n && l > -70.0)
        .map(|(_, l)| l)
        .collect();
    if gated.is_empty() {
        return None;
    }
    let mean = gated.iter().map(|&l| power_from_loudness(l)).sum::<f64>() / gated.len() as f64;
    let relative = loudness_from_power(mean) - 20.0;
    let mut kept: Vec<f64> = gated.into_iter().filter(|&l| l > relative).collect();
    if kept.is_empty() {
        return None;
    }
    kept.sort_by(|a, b| a.total_cmp(b));
    let at = |p: f64| kept[((kept.len() - 1) as f64 * p).round() as usize];
    Some(at(0.95) - at(0.10))
}

/// Taps per phase of the true-peak oversampling FIR.
pub(crate) const TP_TAPS: usize = 12;

/// Peak magnitude around the newest sample `hist[0]`: the sample itself and the three
/// interpolated inter-sample points. `hist` holds the last `TP_TAPS` samples, newest first.
#[inline]
pub(crate) fn oversampled_peak(fir: &[Vec<f64>; 4], hist: &[f64]) -> f64 {
    fir.iter()
        .map(|ph| ph.iter().zip(hist).map(|(h, x)| h * x).sum::<f64>().abs())
        .fold(hist[0].abs(), f64::max)
}

/// Build a 4-phase polyphase FIR for 4x oversampling: a windowed-sinc low-pass at the base
/// Nyquist, split into the four inter-sample phases. Phase coefficients are h[4k + p].
pub(crate) fn design_oversample_fir(taps_per_phase: usize) -> [Vec<f64>; 4] {
    let l = 4usize;
    let n = l * taps_per_phase;
    let fc = 0.25; // base Nyquist, normalized to the oversampled rate (= 1/(2*L)*2)
    let center = (n - 1) as f64 / 2.0;
    let mut h = vec![0.0f64; n];
    for (i, hi) in h.iter_mut().enumerate() {
        let m = i as f64 - center;
        let sinc = if m.abs() < 1e-9 {
            1.0
        } else {
            (PI * fc * m).sin() / (PI * fc * m)
        };
        // Hann window.
        let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / (n - 1) as f64).cos();
        *hi = fc * sinc * w * l as f64;
    }
    let mut phases = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for (p, phase) in phases.iter_mut().enumerate() {
        *phase = (0..taps_per_phase).map(|k| h[l * k + p]).collect();
    }
    phases
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn k_weighting_matches_spec_at_48k() {
        // BS.1770-4 tabulated coefficients at 48 kHz.
        let (s1, s2) = k_weighting(48000.0);
        let near = |a: f64, b: f64| (a - b).abs() < 1e-4;
        assert!(near(s1.b0, 1.53512485958697), "s1.b0 = {}", s1.b0);
        assert!(near(s1.b1, -2.69169618940638), "s1.b1 = {}", s1.b1);
        assert!(near(s1.b2, 1.19839281085285), "s1.b2 = {}", s1.b2);
        assert!(near(s1.a1, -1.69065929318241), "s1.a1 = {}", s1.a1);
        assert!(near(s1.a2, 0.73248077421585), "s1.a2 = {}", s1.a2);
        assert!(near(s2.a1, -1.99004745483398), "s2.a1 = {}", s2.a1);
        assert!(near(s2.a2, 0.99007225036621), "s2.a2 = {}", s2.a2);
    }

    #[test]
    fn stereo_1khz_minus20dbfs_sine_reads_about_minus17_6() {
        // A -20 dBFS RMS 1 kHz sine in BOTH channels: each channel has mean-square 0.01,
        // summed over two 1.0-weighted channels = 0.02, so loudness = -0.691 + 10log10(0.02)
        // ≈ -17.7 LUFS, give or take the K-weighting's small gain at 1 kHz. (Not -20: two
        // equal channels add ~3 dB — this is the correct BS.1770 behaviour.)
        let fs = 48000u32;
        let amp = 10f64.powf(-20.0 / 20.0) * std::f64::consts::SQRT_2; // -20 dBFS RMS
        let mut m = Meter::new(fs, 2, false);
        let mut buf = Vec::new();
        for i in 0..fs as usize * 3 {
            let s = (amp * (2.0 * PI * 1000.0 * i as f64 / fs as f64).sin()) as f32;
            buf.push(s);
            buf.push(s);
        }
        m.push(&buf);
        let l = m.integrated(true).unwrap();
        // ~-17.7 from the channel sum, plus the K-weighting's small boost at 1 kHz.
        assert!((l + 17.0).abs() < 0.6, "measured {l} LUFS, expected ≈ -17.0");
    }
}
