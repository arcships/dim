//! Contrast floors for colours written onto a background the app does not
//! paint (port of `contrast.ts`): a role is held to both a WCAG 2 ratio and an
//! APCA lightness contrast, because no terminal protects 24-bit foregrounds.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Floor {
    pub ratio: f64,
    pub lc: f64,
}

/// Secondary text: WCAG AA for body text, APCA body-text level.
pub const TEXT: Floor = Floor {
    ratio: 4.5,
    lc: 60.0,
};
/// Coloured text (accent, status, syntax).
pub const COLORED_TEXT: Floor = Floor {
    ratio: 4.5,
    lc: 45.0,
};
/// Control boundaries and recessive marks (WCAG 1.4.11).
pub const NON_TEXT: Floor = Floor {
    ratio: 3.0,
    lc: 0.0,
};

pub type Rgb = [f64; 3];

pub fn parse_hex(hex: &str) -> Option<Rgb> {
    let value = hex.strip_prefix('#').unwrap_or(hex);
    if value.len() != 6 {
        return None;
    }
    let channel = |i: usize| {
        u8::from_str_radix(&value[i..i + 2], 16)
            .ok()
            .map(|v| v as f64 / 255.0)
    };
    Some([channel(0)?, channel(2)?, channel(4)?])
}

pub fn to_hex(rgb: Rgb) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (rgb[0] * 255.0).round() as u8,
        (rgb[1] * 255.0).round() as u8,
        (rgb[2] * 255.0).round() as u8
    )
}

fn relative_luminance(rgb: Rgb) -> f64 {
    let [r, g, b] = rgb.map(|c| {
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    });
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn apca_luminance(rgb: Rgb) -> f64 {
    let [r, g, b] = rgb.map(|c| c.powf(2.4));
    0.2126729 * r + 0.7151522 * g + 0.072175 * b
}

fn blend(from: Rgb, to: Rgb, amount: f64) -> Rgb {
    [
        from[0] + (to[0] - from[0]) * amount,
        from[1] + (to[1] - from[1]) * amount,
        from[2] + (to[2] - from[2]) * amount,
    ]
}

/// WCAG 2 contrast ratio.
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (light, dark) = if la > lb { (la, lb) } else { (lb, la) };
    (light + 0.05) / (dark + 0.05)
}

/// Absolute APCA-W3 (0.0.98G-4g) lightness contrast, text on background.
pub fn apca_lightness_contrast(text: Rgb, background: Rgb) -> f64 {
    let clamp = |y: f64| {
        if y > 0.022 {
            y
        } else {
            y + (0.022 - y).powf(1.414)
        }
    };
    let y_text = clamp(apca_luminance(text));
    let y_bg = clamp(apca_luminance(background));
    if (y_bg - y_text).abs() < 0.0005 {
        return 0.0;
    }
    if y_bg > y_text {
        let s = (y_bg.powf(0.56) - y_text.powf(0.57)) * 1.14;
        if s < 0.1 { 0.0 } else { (s - 0.027) * 100.0 }
    } else {
        let s = (y_bg.powf(0.65) - y_text.powf(0.62)) * 1.14;
        if s > -0.1 { 0.0 } else { -(s + 0.027) * 100.0 }
    }
}

fn target_for(background: Rgb) -> Rgb {
    if relative_luminance(background) < 0.18 {
        [1.0, 1.0, 1.0]
    } else {
        [0.0, 0.0, 0.0]
    }
}

/// `color` when it already clears `floor` against `background`; otherwise
/// moved toward white (dark background) or black (light) in 1% steps until it
/// does.
pub fn lift_to_contrast(color: Rgb, background: Rgb, floor: Floor) -> Rgb {
    let target = target_for(background);
    for step in 0..=100 {
        let candidate = if step == 0 {
            color
        } else {
            blend(color, target, step as f64 / 100.0)
        };
        // Quantize like the hex round trip the TS version goes through.
        let candidate = parse_hex(&to_hex(candidate)).unwrap_or(candidate);
        if contrast_ratio(candidate, background) >= floor.ratio
            && apca_lightness_contrast(candidate, background) >= floor.lc
        {
            return candidate;
        }
    }
    target
}

const SURFACE_RATIO_MIN: f64 = 1.12;
const SURFACE_RATIO_MAX: f64 = 1.5;
const SURFACE_RATIO_TARGET: f64 = 1.18;

/// Keeps `surface` a visible step off `background`.
pub fn surface_for_background(surface: Rgb, background: Rgb) -> Rgb {
    let ratio = contrast_ratio(surface, background);
    if (SURFACE_RATIO_MIN..=SURFACE_RATIO_MAX).contains(&ratio) {
        return surface;
    }
    let target = target_for(background);
    for step in 1..=100 {
        let candidate = blend(background, target, step as f64 / 100.0);
        if contrast_ratio(candidate, background) >= SURFACE_RATIO_TARGET {
            return candidate;
        }
    }
    target
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(hex: &str) -> Rgb {
        parse_hex(hex).unwrap()
    }

    #[test]
    fn black_on_white_is_21_to_1() {
        let ratio = contrast_ratio(rgb("#000000"), rgb("#ffffff"));
        assert!((ratio - 21.0).abs() < 1e-9);
    }

    #[test]
    fn apca_is_about_106_for_black_on_white() {
        let lc = apca_lightness_contrast(rgb("#000000"), rgb("#ffffff"));
        assert!((lc - 106.0).abs() < 1.0, "{lc}");
    }

    #[test]
    fn a_colour_that_clears_its_floor_is_untouched() {
        let color = rgb("#e4e4e7");
        assert_eq!(lift_to_contrast(color, rgb("#282828"), TEXT), color);
    }

    #[test]
    fn a_weak_colour_is_lifted_toward_the_foreground_side() {
        // Dim green on white fails; it moves toward black until it passes.
        let lifted = lift_to_contrast(rgb("#a9dc76"), rgb("#ffffff"), COLORED_TEXT);
        assert!(contrast_ratio(lifted, rgb("#ffffff")) >= 4.5);
        assert!(lifted[0] < rgb("#a9dc76")[0]);
    }

    #[test]
    fn surface_stays_a_visible_step_off_the_background() {
        let surface = surface_for_background(rgb("#ffffff"), rgb("#ffffff"));
        let ratio = contrast_ratio(surface, rgb("#ffffff"));
        assert!(ratio >= SURFACE_RATIO_TARGET, "{ratio}");
    }
}
