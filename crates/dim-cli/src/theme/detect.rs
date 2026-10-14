//! Terminal capability and background detection (ports of
//! `detectTerminalCapability` and `terminal-appearance.ts`). Both are decided
//! once, before the first line is written: committed scrollback cannot be
//! repainted.

use super::palette::{Appearance, ColorMode};
use std::collections::HashMap;

/// OKLCH lightness above which a background takes the light palette.
const LIGHT_THRESHOLD_L: f64 = 0.66;

pub type Env = HashMap<String, String>;

pub fn env_from_process() -> Env {
    std::env::vars().collect()
}

const TRUECOLOR_PROGRAMS: [&str; 9] = [
    "iterm.app",
    "wezterm",
    "vscode",
    "warpterminal",
    "ghostty",
    "rio",
    "alacritty",
    "kitty",
    "windows_terminal",
];

/// Colour depth from the environment.
pub fn color_mode(env: &Env, is_tty: bool) -> ColorMode {
    if !is_tty {
        return ColorMode::TrueColor;
    }
    let lower = |key: &str| env.get(key).map(|v| v.to_lowercase()).unwrap_or_default();
    let colorterm = lower("COLORTERM");
    let term = lower("TERM");
    let program = lower("TERM_PROGRAM");
    let emulator = lower("TERMINAL_EMULATOR");
    if colorterm.contains("truecolor")
        || colorterm.contains("24bit")
        || term.contains("truecolor")
        || term.contains("24bit")
        || term.contains("direct")
        || env.contains_key("WT_SESSION")
        || TRUECOLOR_PROGRAMS.contains(&program.as_str())
        || emulator == "jetbrains-jediterm"
    {
        return ColorMode::TrueColor;
    }
    if term.contains("256color") {
        return ColorMode::Ansi256;
    }
    // `dumb`, `vt100` and friends have 8 colours at best; they share the
    // 16-colour palette like every other low-colour terminal.
    ColorMode::Ansi16
}

/// `DIM_TERMINAL_APPEARANCE=light|dark`; anything else is ignored.
pub fn appearance_from_override(env: &Env) -> Option<Appearance> {
    let value = env
        .get("DIM_TERMINAL_APPEARANCE")
        .or_else(|| env.get("DIMCODE_TERMINAL_APPEARANCE"))?
        .trim()
        .to_lowercase();
    match value.as_str() {
        "light" => Some(Appearance::Light),
        "dark" => Some(Appearance::Dark),
        _ => None,
    }
}

/// `COLORFGBG` is `fg;bg` (sometimes `fg;<extra>;bg`); only the trailing
/// background index counts: 0-6 and 8 dark, 7 and 9-15 light.
pub fn appearance_from_colorfgbg(env: &Env) -> Option<Appearance> {
    let raw = env.get("COLORFGBG")?;
    let last = raw.split(';').next_back()?.trim();
    if last.is_empty() || !last.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let index: u32 = last.parse().ok()?;
    if index > 15 {
        return None;
    }
    Some(if index == 7 || index > 8 {
        Appearance::Light
    } else {
        Appearance::Dark
    })
}

/// OKLCH lightness of an `#rrggbb` colour.
pub fn background_lightness(hex: &str) -> f64 {
    let value = hex.trim_start_matches('#');
    let channel = |i: usize| {
        let raw = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).unwrap_or(0) as f64 / 255.0;
        if raw <= 0.04045 {
            raw / 12.92
        } else {
            ((raw + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (channel(0), channel(1), channel(2));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s
}

pub fn appearance_for_background(hex: &str) -> Appearance {
    if background_lightness(hex) >= LIGHT_THRESHOLD_L {
        Appearance::Light
    } else {
        Appearance::Dark
    }
}

/// Parses one OSC 11 report body (`rgb:RRRR/GGGG/BBBB`, any hex width per
/// channel, optional alpha) into `#rrggbb`. Each channel is scaled by its own
/// width, so a 4-bit terminal does not read as near-black.
pub fn parse_background_report(body: &str) -> Option<String> {
    let rest = body
        .strip_prefix("rgb:")
        .or_else(|| body.strip_prefix("rgba:"))?;
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() < 3 {
        return None;
    }
    let mut out = String::from("#");
    for part in &parts[..3] {
        if part.is_empty() || part.len() > 4 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let max = 16f64.powi(part.len() as i32) - 1.0;
        let value = u32::from_str_radix(part, 16).ok()? as f64;
        out.push_str(&format!("{:02x}", (value / max * 255.0).round() as u8));
    }
    Some(out)
}

/// Resolution order: override, OSC 11 report, `COLORFGBG`, dark.
pub fn resolve_appearance(
    env: &Env,
    osc11_background: Option<&str>,
) -> (Appearance, Option<String>) {
    if let Some(appearance) = appearance_from_override(env) {
        return (appearance, osc11_background.map(str::to_owned));
    }
    if let Some(hex) = osc11_background {
        return (appearance_for_background(hex), Some(hex.to_owned()));
    }
    if let Some(appearance) = appearance_from_colorfgbg(env) {
        return (appearance, None);
    }
    (Appearance::Dark, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn colour_depth_follows_the_capability_rules() {
        assert_eq!(
            color_mode(&env(&[("COLORTERM", "truecolor")]), true),
            ColorMode::TrueColor
        );
        assert_eq!(
            color_mode(&env(&[("TERM_PROGRAM", "ghostty")]), true),
            ColorMode::TrueColor
        );
        assert_eq!(
            color_mode(&env(&[("TERM", "xterm-256color")]), true),
            ColorMode::Ansi256
        );
        assert_eq!(
            color_mode(&env(&[("TERM", "xterm")]), true),
            ColorMode::Ansi16
        );
        assert_eq!(
            color_mode(&env(&[("TERM", "dumb")]), true),
            ColorMode::Ansi16
        );
        assert_eq!(color_mode(&env(&[]), false), ColorMode::TrueColor);
    }

    #[test]
    fn appearance_precedence_override_osc11_colorfgbg_dark() {
        let light_bg = Some("#fafafa");
        let e = env(&[("DIM_TERMINAL_APPEARANCE", "dark"), ("COLORFGBG", "0;15")]);
        assert_eq!(resolve_appearance(&e, light_bg).0, Appearance::Dark);
        let e = env(&[("COLORFGBG", "0;0")]);
        assert_eq!(resolve_appearance(&e, light_bg).0, Appearance::Light);
        assert_eq!(resolve_appearance(&e, None).0, Appearance::Dark);
        assert_eq!(
            resolve_appearance(&env(&[("COLORFGBG", "0;15")]), None).0,
            Appearance::Light
        );
        assert_eq!(resolve_appearance(&env(&[]), None).0, Appearance::Dark);
        // Not a palette index: ignored rather than guessed at.
        assert_eq!(
            resolve_appearance(&env(&[("COLORFGBG", "0;default")]), None).0,
            Appearance::Dark
        );
    }

    #[test]
    fn osc11_channels_scale_by_their_own_width() {
        assert_eq!(
            parse_background_report("rgb:ffff/ffff/ffff").as_deref(),
            Some("#ffffff")
        );
        assert_eq!(
            parse_background_report("rgb:f/f/f").as_deref(),
            Some("#ffffff")
        );
        assert_eq!(
            parse_background_report("rgb:0000/0000/0000").as_deref(),
            Some("#000000")
        );
        assert_eq!(
            parse_background_report("rgb:8080/8080/8080/ffff").as_deref(),
            Some("#808080")
        );
        assert_eq!(parse_background_report("nonsense"), None);
    }

    #[test]
    fn lightness_splits_dark_and_light_backgrounds() {
        assert_eq!(appearance_for_background("#1e1e1e"), Appearance::Dark);
        assert_eq!(appearance_for_background("#ffffff"), Appearance::Light);
        assert_eq!(appearance_for_background("#f5f5f5"), Appearance::Light);
    }
}
