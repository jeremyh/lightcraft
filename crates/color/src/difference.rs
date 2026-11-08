//! Colour difference: CIELAB from XYZ and the CIEDE2000 difference between two CIELAB colours.

/// CIELAB of `xyz` relative to the reference white `white` (both XYZ, white with Y = 1).
pub fn lab(xyz: [f64; 3], white: [f64; 3]) -> [f64; 3] {
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let ratio = |i: usize| if white[i] > 0.0 { xyz[i] / white[i] } else { 0.0 };
    let [fx, fy, fz] = [f(ratio(0)), f(ratio(1)), f(ratio(2))];
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIEDE2000 (Sharma, Wu and Dalal 2005).
pub fn ciede2000(a: [f64; 3], b: [f64; 3]) -> f64 {
    use std::f64::consts::PI;
    let (l1, a1, b1, l2, a2, b2) = (a[0], a[1], a[2], b[0], b[1], b[2]);
    let c_bar = ((a1 * a1 + b1 * b1).sqrt() + (a2 * a2 + b2 * b2).sqrt()) / 2.0;
    let g = 0.5 * (1.0 - (c_bar.powi(7) / (c_bar.powi(7) + 25f64.powi(7))).sqrt());
    let (a1p, a2p) = ((1.0 + g) * a1, (1.0 + g) * a2);
    let (c1p, c2p) = ((a1p * a1p + b1 * b1).sqrt(), (a2p * a2p + b2 * b2).sqrt());
    let hue = |b: f64, a: f64| if b == 0.0 && a == 0.0 { 0.0 } else { b.atan2(a).rem_euclid(2.0 * PI) };
    let (h1p, h2p) = (hue(b1, a1p), hue(b2, a2p));
    let dl = l2 - l1;
    let dc = c2p - c1p;
    let dh = if c1p * c2p == 0.0 {
        0.0
    } else {
        let d = h2p - h1p;
        if d.abs() <= PI {
            d
        } else if d > PI {
            d - 2.0 * PI
        } else {
            d + 2.0 * PI
        }
    };
    let dhh = 2.0 * (c1p * c2p).sqrt() * (dh / 2.0).sin();
    let l_bar = (l1 + l2) / 2.0;
    let cp_bar = (c1p + c2p) / 2.0;
    let hp_bar = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= PI {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 2.0 * PI {
        (h1p + h2p + 2.0 * PI) / 2.0
    } else {
        (h1p + h2p - 2.0 * PI) / 2.0
    };
    let t = 1.0 - 0.17 * (hp_bar - PI / 6.0).cos() + 0.24 * (2.0 * hp_bar).cos() + 0.32 * (3.0 * hp_bar + PI / 30.0).cos()
        - 0.20 * (4.0 * hp_bar - 63.0 * PI / 180.0).cos();
    let d_theta = 30.0 * (-(((hp_bar * 180.0 / PI) - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (cp_bar.powi(7) / (cp_bar.powi(7) + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (l_bar - 50.0).powi(2) / (20.0 + (l_bar - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cp_bar;
    let sh = 1.0 + 0.015 * cp_bar * t;
    let rt = -(2.0 * d_theta * PI / 180.0).sin() * rc;
    ((dl / sl).powi(2) + (dc / sc).powi(2) + (dhh / sh).powi(2) + rt * (dc / sc) * (dhh / sh)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ciede2000_matches_the_published_test_pairs() {
        // Sharma, Wu and Dalal (2005), table 1: pairs 1, 7 and 17
        let cases = [
            ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
            ([50.0, 0.0, 0.0], [50.0, -1.0, 2.0], 2.3669),
            ([50.0, 2.5, 0.0], [73.0, 25.0, -18.0], 27.1492),
        ];
        for (a, b, e) in cases {
            assert!((ciede2000(a, b) - e).abs() < 1e-3, "{a:?} {b:?}: {} vs {e}", ciede2000(a, b));
        }
    }

    #[test]
    fn white_is_lightness_100_and_grey_has_no_colour() {
        let white = [0.95047, 1.0, 1.08883];
        let l = lab(white, white);
        assert!((l[0] - 100.0).abs() < 1e-9 && l[1].abs() < 1e-9 && l[2].abs() < 1e-9);
        let grey = lab(white.map(|v| v * 0.18), white);
        assert!(grey[1].abs() < 1e-9 && grey[2].abs() < 1e-9);
        assert_eq!(ciede2000(grey, grey), 0.0);
        // a degenerate white gives a finite colour, not NaN
        assert!(lab([0.5; 3], [0.0; 3]).iter().all(|v| v.is_finite()));
    }
}
