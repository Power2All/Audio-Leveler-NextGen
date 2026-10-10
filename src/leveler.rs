//! Adaptive loudness leveler — an original implementation of the general technique
//! (time-varying gain driven by a short-term loudness estimate), not a port of anyone
//! else's code.
//!
//! The idea: track how loud the programme is over a sliding window, and gently pull each
//! moment toward a single reference loudness — lifting quiet passages, easing loud ones —
//! so the whole piece sits at an even level without the fast gain-riding of a compressor.
//! Two things keep it musical:
//!   * the gain change per moment is clamped to a limited range and heavily time-smoothed,
//!     so micro-dynamics and transients survive;
//!   * a silence/noise gate stops it from boosting the near-silent gaps between tracks.
//!
//! After leveling, the pipeline measures the result and applies one constant make-up gain
//! so the integrated loudness lands exactly on the user's target.

use crate::loudness;

/// Tunable leveler behaviour. `strength` in the CLI maps to one of these.
#[derive(Clone, Copy)]
pub struct Params {
    pub window_s: f64,     // sliding window for the loudness estimate
    pub max_boost_db: f64, // most we will lift a quiet moment
    pub max_cut_db: f64,   // most we will pull down a loud moment
    pub pull: f64,         // 0..1: how far toward the reference we move
    pub attack_s: f64,     // gain-reduction time constant (fast)
    pub release_s: f64,    // gain-recovery time constant (slow)
}

/// Apply adaptive leveling in place.
///
/// * `reference_lufs` — the loudness every moment is pulled toward (we use the file's own
///   gated integrated loudness, so leveling evens out variation without shifting the
///   overall level; the make-up gain afterward sets the absolute target).
/// * `noise_floor_dbfs` — moments near or below this are treated as silence and not boosted.
pub fn apply(
    channels: &mut [Vec<f32>],
    fs: u32,
    reference_lufs: f64,
    noise_floor_dbfs: f64,
    p: &Params,
) {
    let n = channels.first().map(|c| c.len()).unwrap_or(0);
    if n == 0 {
        return;
    }
    let hop = ((fs as f64) * 0.1).round().max(1.0) as usize; // 100 ms control rate
    let win = ((fs as f64) * p.window_s).round().max(hop as f64) as usize;
    let env = loudness::short_term_loudness(channels, fs, win, hop);
    if env.is_empty() {
        return;
    }

    // Anything within 6 dB of the noise floor, or below the -70 LUFS absolute gate, is
    // "silence": desired gain 0 dB so we never amplify hiss in the gaps.
    let silence_gate = (noise_floor_dbfs + 6.0).max(-70.0);

    // Desired per-hop gain (dB), pulling toward the reference, range-clamped and gated.
    let desired: Vec<f64> = env
        .iter()
        .map(|&l| {
            if l < silence_gate {
                0.0
            } else {
                (p.pull * (reference_lufs - l)).clamp(-p.max_cut_db, p.max_boost_db)
            }
        })
        .collect();

    // Asymmetric one-pole smoothing of the gain: quick to pull down, slow to bring back up.
    let att = 1.0 - (-(hop as f64) / (p.attack_s * fs as f64)).exp();
    let rel = 1.0 - (-(hop as f64) / (p.release_s * fs as f64)).exp();
    let mut smooth = vec![0.0f64; desired.len()];
    let mut g = desired[0];
    for (i, &d) in desired.iter().enumerate() {
        let coef = if d < g { att } else { rel };
        g += coef * (d - g);
        smooth[i] = g;
    }

    // Per-sample linear gain, linearly interpolating the per-hop dB gain. Computed once and
    // shared by every channel (the pow is the expensive part).
    let gain: Vec<f32> = (0..n)
        .map(|i| {
            let pos = i as f64 / hop as f64;
            let k = pos.floor() as usize;
            let frac = pos - k as f64;
            let g_db = if k + 1 < smooth.len() {
                smooth[k] * (1.0 - frac) + smooth[k + 1] * frac
            } else {
                smooth[smooth.len() - 1]
            };
            10f64.powf(g_db / 20.0) as f32
        })
        .collect();
    for ch in channels.iter_mut() {
        for (s, g) in ch.iter_mut().zip(&gain) {
            *s *= g;
        }
    }
}
