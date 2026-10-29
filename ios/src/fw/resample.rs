//! Band-limited resampling: the weights a real mixer uses (a windowed sinc)
//! to read a sound between its samples, so a slow-rate sound played at the
//! output's rate neither loses its highs nor gains mirror tones above them.

use std::sync::OnceLock;

/// Zero crossings of the sinc on each side.
const ZEROS: usize = 32;
/// Table points per sample.
const STEPS: usize = 256;
/// Passband as a share of the lower rate's half (room for the filter's slope).
const PASS: f64 = 0.9;

/// The windowed sinc from 0 to ZEROS (Kaiser window, beta 7: ~70 dB down).
fn table() -> &'static [f32] {
    static TABLE: OnceLock<Vec<f32>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let beta = 7.0;
        (0..=ZEROS * STEPS + 1)
            .map(|i| {
                let x = i as f64 / STEPS as f64;
                let sinc = if x == 0.0 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
                let r = (x / ZEROS as f64).min(1.0);
                (sinc * bessel_i0(beta * (1.0 - r * r).sqrt()) / bessel_i0(beta)) as f32
            })
            .collect()
    })
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > sum * 1e-12 {
        term *= (x / (2.0 * k)) * (x / (2.0 * k));
        sum += term;
        k += 1.0;
    }
    sum
}

fn kernel(x: f64) -> f32 {
    let at = x.abs() * STEPS as f64;
    let index = at as usize;
    let table = table();
    if index >= ZEROS * STEPS {
        return 0.0;
    }
    let part = (at - index as f64) as f32;
    table[index] + (table[index + 1] - table[index]) * part
}

/// Reads a sound at input position `at` (in its frames) when it moves `step`
/// input frames per output frame; `frame(i)` gives frame `i` (any i, zero
/// outside the sound). Returns (left, right).
pub fn read(at: f64, step: f64, frame: impl Fn(isize) -> (f32, f32)) -> (f32, f32) {
    // Faster than the output: cut below the output's half rate instead.
    let scale = PASS * (1.0 / step).min(1.0);
    let reach = (ZEROS as f64 / scale).ceil() as isize;
    let base = at.floor() as isize;
    let (mut left, mut right, mut total) = (0.0f32, 0.0f32, 0.0f32);
    for i in base - reach + 1..=base + reach {
        let weight = kernel((at - i as f64) * scale);
        if weight != 0.0 {
            let (l, r) = frame(i);
            left += l * weight;
            right += r * weight;
            total += weight;
        }
    }
    // Weights sum to ~1; dividing removes the small ripple between positions.
    if total.abs() > 1e-6 {
        (left / total, right / total)
    } else {
        (0.0, 0.0)
    }
}
