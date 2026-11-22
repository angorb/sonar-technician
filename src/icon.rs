//! The tray icon, drawn in code (no resource file): a volume dial, i.e. a 270°
//! track with a filled arc and a centre dot. Pure pixel math so it's tested on
//! any platform; `platform::tray` turns the pixels into an `HICON`.
//!
//! Geometry is defined on a 32-unit grid and scaled to the icon size.

/// Orange when connected to Sonar.
pub const CONNECTED: (u8, u8, u8) = (0xFF, 0x6A, 0x13);
/// Grey when Sonar is unavailable.
pub const OFFLINE: (u8, u8, u8) = (0x8A, 0x8A, 0x8A);

const TRACK: (u8, u8, u8) = (0x8A, 0x8A, 0x8A);
const TRACK_ALPHA: f32 = 0.45;
/// Arc starts bottom-left (135°, screen coordinates: y down) and runs clockwise.
const START_DEG: f32 = 135.0;
const TRACK_SWEEP: f32 = 270.0;
/// How much of the track the filled arc covers (purely decorative).
const FILL_SWEEP: f32 = 270.0 * 0.65;

/// Coverage (0..1) of a pixel at distance `d` from an edge at `r` (inside = d < r).
fn cover(d: f32, r: f32) -> f32 {
    (r - d + 0.5).clamp(0.0, 1.0)
}

/// Coverage of a round-capped arc stroke at pixel offset (x, y) from the centre.
fn arc(x: f32, y: f32, radius: f32, half_w: f32, sweep: f32) -> f32 {
    let d = (x * x + y * y).sqrt();
    let angle = y.atan2(x).to_degrees();
    let rel = (angle - START_DEG).rem_euclid(360.0);
    let band = if rel <= sweep {
        cover((d - radius).abs(), half_w)
    } else {
        0.0
    };
    let cap = |deg: f32| {
        let (s, c) = deg.to_radians().sin_cos();
        let (ex, ey) = (radius * c, radius * s);
        cover(((x - ex).powi(2) + (y - ey).powi(2)).sqrt(), half_w)
    };
    band.max(cap(START_DEG)).max(cap(START_DEG + sweep))
}

/// Premultiplied BGRA pixels (`0xAARRGGBB`), top-down, `n` x `n`.
pub fn dial(n: i32, color: (u8, u8, u8)) -> Vec<u32> {
    let n = n.max(16);
    let unit = n as f32 / 32.0;
    let (radius, half_w, dot) = (11.0 * unit, 2.0 * unit, 3.0 * unit);
    let c = n as f32 / 2.0;
    let mut px = Vec::with_capacity((n * n) as usize);
    for py in 0..n {
        for pxi in 0..n {
            let (x, y) = (pxi as f32 + 0.5 - c, py as f32 + 0.5 - c);
            // Layers bottom to top: (rgb, alpha) composited "over".
            let layers = [
                (TRACK, arc(x, y, radius, half_w, TRACK_SWEEP) * TRACK_ALPHA),
                (color, arc(x, y, radius, half_w, FILL_SWEEP)),
                (color, cover((x * x + y * y).sqrt(), dot)),
            ];
            let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0, 0.0, 0.0);
            for ((lr, lg, lb), la) in layers {
                r = f32::from(lr) * la + r * (1.0 - la);
                g = f32::from(lg) * la + g * (1.0 - la);
                b = f32::from(lb) * la + b * (1.0 - la);
                a = la + a * (1.0 - la);
            }
            let q = |v: f32| (v.round() as u32).min(255);
            px.push((q(a * 255.0) << 24) | (q(r) << 16) | (q(g) << 8) | q(b));
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(px: &[u32], n: i32, x: i32, y: i32) -> (u32, u32) {
        let p = px[(y * n + x) as usize];
        (p >> 24, p & 0xFF_FFFF)
    }

    #[test]
    fn dial_shape() {
        let n = 32;
        let px = dial(n, CONNECTED);
        let orange = 0xFF6A13;
        // Centre dot: opaque orange.
        assert_eq!(at(&px, n, 16, 16), (255, orange));
        // Corners and the gap at the bottom are transparent.
        assert_eq!(at(&px, n, 0, 0).0, 0);
        assert_eq!(at(&px, n, 16, 27).0, 0, "open at the bottom");
        // Filled arc at the left (180°) and top (270°).
        assert_eq!(at(&px, n, 5, 16), (255, orange));
        assert_eq!(at(&px, n, 16, 5), (255, orange));
        // Right side (0°) is past the fill: translucent grey track only.
        let (a, _) = at(&px, n, 27, 16);
        assert!(a > 60 && a < 160, "track alpha {a}");
    }

    #[test]
    fn premultiplied_and_scaled() {
        for n in [16, 20, 24, 32, 48] {
            let px = dial(n, OFFLINE);
            assert_eq!(px.len(), (n * n) as usize);
            for p in px {
                let a = p >> 24;
                for s in [0, 8, 16] {
                    assert!((p >> s) & 0xFF <= a, "n={n}: colour exceeds alpha");
                }
            }
        }
    }
}
