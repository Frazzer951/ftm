//! Color math for workspace colors ftmd picks itself: `random` and the color a child takes from its parent.

/// Saturation and lightness a picked color stays within, so it reads as a color (not near gray, white or black) on
/// light and dark themes alike.
pub const SATURATION: (f64, f64) = (0.55, 0.85);
pub const LIGHTNESS: (f64, f64) = (0.45, 0.65);

/// Hue and lightness steps for a parent's `n`th child, cycled: each reads as the parent's color nudged, and no two in
/// a cycle land on the same color.
const CHILD_STEPS: [(f64, f64); 6] = [
    (16.0, 0.06),
    (-16.0, -0.06),
    (32.0, 0.0),
    (-32.0, 0.0),
    (16.0, -0.06),
    (-16.0, 0.06),
];

pub fn hsl(hex: &str) -> Option<(f64, f64, f64)> {
    let hex = hex.strip_prefix('#').filter(|h| h.len() == 6)?;
    let n = u32::from_str_radix(hex, 16).ok()?;
    let [r, g, b] = [(n >> 16) & 255, (n >> 8) & 255, n & 255].map(|v| f64::from(v) / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let d = max - min;
    if d == 0.0 {
        return Some((0.0, 0.0, l));
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    Some((h * 60.0, s, l))
}

pub fn hex(h: f64, s: f64, l: f64) -> String {
    let h = h.rem_euclid(360.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match h {
        h if h < 60.0 => (c, x, 0.0),
        h if h < 120.0 => (x, c, 0.0),
        h if h < 180.0 => (0.0, c, x),
        h if h < 240.0 => (0.0, x, c),
        h if h < 300.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let byte = |v: f64| {
        let v = ((v + m) * 255.0).round();
        (0..=u8::MAX).find(|&b| f64::from(b) >= v).unwrap_or(u8::MAX)
    };
    format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b))
}

/// A color in the readable band from 64 random bits.
pub fn random_color(bits: u64) -> String {
    let b = bits.to_le_bytes();
    let unit = |i: usize| f64::from(u16::from_le_bytes([b[i], b[i + 1]])) / f64::from(u16::MAX);
    let h = unit(0) * 360.0;
    let s = SATURATION.0 + unit(2) * (SATURATION.1 - SATURATION.0);
    let l = LIGHTNESS.0 + unit(4) * (LIGHTNESS.1 - LIGHTNESS.0);
    hex(h, s, l)
}

/// The color for a parent's `n`th child: the parent's `#rrggbb` with its hue and lightness nudged, lightness kept in
/// the readable band. Saturation stays the parent's, so a gray parent has grayish children.
pub fn child_color(parent: &str, n: usize) -> Option<String> {
    let (h, s, l) = hsl(parent)?;
    let (dh, dl) = CHILD_STEPS[n % CHILD_STEPS.len()];
    // Later cycles drift a little further round the wheel so they don't repeat the first.
    let drift = (n / CHILD_STEPS.len()) as f64 * 7.0 * dh.signum();
    Some(hex(h + dh + drift, s, (l + dl).clamp(LIGHTNESS.0, LIGHTNESS.1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_band(hex: &str) -> bool {
        let (_, s, l) = hsl(hex).unwrap();
        let eps = 0.01;
        (SATURATION.0 - eps..=SATURATION.1 + eps).contains(&s) && (LIGHTNESS.0 - eps..=LIGHTNESS.1 + eps).contains(&l)
    }

    #[test]
    fn hsl_round_trips() {
        for c in ["#ff4245", "#0091ff", "#98989d", "#000000", "#ffffff", "#30d158"] {
            let (h, s, l) = hsl(c).unwrap();
            assert_eq!(hex(h, s, l), c);
        }
        assert_eq!(hsl("#abc"), None);
        assert_eq!(hsl("red"), None);
    }

    #[test]
    fn random_colors_stay_readable() {
        let mut bits = 0x9e37_79b9_7f4a_7c15_u64;
        for _ in 0..1000 {
            bits = bits
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let c = random_color(bits);
            assert!(in_band(&c), "{c}");
        }
        assert!(in_band(&random_color(0)) && in_band(&random_color(u64::MAX)));
    }

    #[test]
    fn children_are_near_the_parent_and_differ() {
        let parent = "#0091ff";
        let (ph, ..) = hsl(parent).unwrap();
        let kids: Vec<String> = (0..12).map(|n| child_color(parent, n).unwrap()).collect();
        for (i, k) in kids.iter().enumerate() {
            assert_ne!(k, parent);
            let (h, _, l) = hsl(k).unwrap();
            let dist = (h - ph).rem_euclid(360.0).min((ph - h).rem_euclid(360.0));
            assert!(dist <= 50.0, "{k} is {dist}° from {parent}");
            assert!((LIGHTNESS.0 - 0.01..=LIGHTNESS.1 + 0.01).contains(&l), "{k}");
            assert!(!kids[..i].contains(k), "{k} repeats a sibling");
        }
        assert_eq!(child_color("blue", 0), None);
    }

    #[test]
    fn hue_shifts_wrap() {
        let red = hex(355.0, 0.8, 0.55);
        let (h, ..) = hsl(&child_color(&red, 0).unwrap()).unwrap();
        assert!((h - 11.0).abs() < 1.5, "{h}");
        let (h, ..) = hsl(&child_color(&hex(5.0, 0.8, 0.55), 1).unwrap()).unwrap();
        assert!((h - 349.0).abs() < 1.5, "{h}");
    }

    #[test]
    fn a_gray_parent_has_grayish_children() {
        let (_, s, _) = hsl(&child_color("#98989d", 0).unwrap()).unwrap();
        assert!(s < 0.1, "{s}");
    }
}
