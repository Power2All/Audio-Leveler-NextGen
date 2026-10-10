//! Look-ahead true-peak limiter — original implementation.
//!
//! After make-up gain the signal may exceed the true-peak ceiling (including inter-sample
//! peaks). We detect the 4x-oversampled peak around every sample, derive the gain needed
//! to stay under the ceiling, take a sliding minimum over a short look-ahead so the gain
//! is already down before a peak arrives, then smooth it (fast attack / slow release) and
//! apply the same gain to every channel so the stereo image is preserved.

use crate::loudness;

/// Limit the true peak to `ceiling_dbtp`. Operates in place.
pub fn apply(channels: &mut [Vec<f32>], fs: u32, ceiling_dbtp: f64) {
    let n = channels.first().map(|c| c.len()).unwrap_or(0);
    if n == 0 {
        return;
    }
    // Aim 0.3 dB under the ceiling so smoothing/rounding can't nudge us back over it.
    let ceiling = 10f64.powf((ceiling_dbtp - 0.3) / 20.0);
    let fir = loudness::design_oversample_fir(loudness::TP_TAPS);

    // Per-sample oversampled peak magnitude, max across channels.
    let mut pk = vec![0.0f64; n];
    for ch in channels.iter() {
        let mut hist = vec![0.0f64; loudness::TP_TAPS];
        for (i, &s) in ch.iter().enumerate() {
            hist.rotate_right(1);
            hist[0] = s as f64;
            pk[i] = pk[i].max(loudness::oversampled_peak(&fir, &hist));
        }
    }

    // Required gain per sample to meet the ceiling.
    let req: Vec<f64> = pk
        .iter()
        .map(|&v| if v > ceiling { ceiling / v } else { 1.0 })
        .collect();

    // Look-ahead: take the sliding minimum of the required gain over the next ~1.5 ms, so
    // the limiter starts ducking before the peak. Computed with a monotonic deque.
    let la = ((fs as f64) * 0.0015).round().max(1.0) as usize;
    let mut look = vec![1.0f64; n];
    {
        use std::collections::VecDeque;
        let mut dq: VecDeque<usize> = VecDeque::new();
        // Process from the right so each position sees [i, i+la].
        for i in (0..n).rev() {
            while let Some(&b) = dq.back() {
                if req[b] >= req[i] {
                    dq.pop_back();
                } else {
                    break;
                }
            }
            dq.push_back(i);
            if let Some(&f) = dq.front() {
                if f > i + la {
                    dq.pop_front();
                }
            }
            look[i] = req[*dq.front().unwrap()];
        }
    }

    // Fast attack, slow release smoothing of the gain envelope.
    let att = 1.0 - (-1.0 / (fs as f64 * 0.0005)).exp(); // ~0.5 ms
    let rel = 1.0 - (-1.0 / (fs as f64 * 0.05)).exp(); // ~50 ms
    let mut env = vec![1.0f64; n];
    // Start at the first look-ahead gain, not 1.0, so a peak at the very start is caught.
    let mut g = look[0];
    for i in 0..n {
        let t = look[i];
        let coef = if t < g { att } else { rel };
        g += coef * (t - g);
        // The one-pole attack may not have fallen far enough by the time a sharp peak
        // arrives; never exceed the gain the ceiling requires.
        env[i] = g.min(req[i]);
    }

    for ch in channels.iter_mut() {
        for i in 0..n {
            ch[i] = (ch[i] as f64 * env[i]) as f32;
        }
    }
}
