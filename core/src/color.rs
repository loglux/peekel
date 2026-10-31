//! Colours shared by the document formats: red, green, blue to hue, saturation, lightness and back, and the lightening and
//! darkening by a tint that spreadsheets use.

use crate::page::Rgb;

/// The colour a six-digit hexadecimal value says (`4472C4`); anything else is `None`.
pub fn hex(value: Option<&str>) -> Option<Rgb> {
    let v = value?;
    if v.len() != 6 {
        return None;
    }
    let part = |i: usize| u8::from_str_radix(v.get(i..i + 2)?, 16).ok();
    Some([part(0)?, part(2)?, part(4)?])
}

pub fn to_hsl(c: Rgb) -> (f32, f32, f32) {
    let (r, g, b) = (
        f32::from(c[0]) / 255.0,
        f32::from(c[1]) / 255.0,
        f32::from(c[2]) / 255.0,
    );
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    if (max - min).abs() < f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if (max - r).abs() < f32::EPSILON {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if (max - g).abs() < f32::EPSILON {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

pub fn from_hsl(h: f32, s: f32, l: f32) -> Rgb {
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let channel = |t: f32| {
        let t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return [v, v, v];
    }
    [
        channel(h / 360.0 + 1.0 / 3.0),
        channel(h / 360.0),
        channel(h / 360.0 - 1.0 / 3.0),
    ]
}

/// `colour` made lighter (`tint` above 0) or darker (below 0) as a spreadsheet does: the lightness moves that share of the
/// way to white or to black.
pub fn tinted(colour: Rgb, tint: f32) -> Rgb {
    let (h, s, l) = to_hsl(colour);
    let l = if tint < 0.0 {
        l * (1.0 + tint)
    } else {
        l * (1.0 - tint) + tint
    };
    from_hsl(h, s, l.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hue_saturation_lightness_go_there_and_back() {
        for c in [
            [68u8, 114, 196],
            [0, 0, 0],
            [255, 255, 255],
            [200, 30, 30],
            [10, 200, 90],
            [128, 128, 128],
        ] {
            let (h, s, l) = to_hsl(c);
            let back = from_hsl(h, s, l);
            assert!(back.iter().zip(c).all(|(a, b)| a.abs_diff(b) <= 1), "{c:?} -> {back:?}");
        }
    }

    #[test]
    fn a_tint_moves_the_lightness_toward_white_or_black() {
        assert_eq!(tinted([100, 100, 100], 0.0), [100, 100, 100]);
        let lighter = tinted([68, 114, 196], 0.6);
        let darker = tinted([68, 114, 196], -0.5);
        assert!(lighter[0] > 150 && darker[2] < 110, "{lighter:?} {darker:?}");
        assert_eq!(tinted([10, 20, 30], 1.0), [255, 255, 255]);
        assert_eq!(tinted([10, 20, 30], -1.0), [0, 0, 0]);
    }
}
