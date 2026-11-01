//! Colour roles of the terminal interface, named themes with contrast floors
//! against the real background, and default theme detection.

pub mod ansi;
pub mod contrast;
pub mod detect;
pub mod palette;
pub mod themes;

use contrast::{
    COLORED_TEXT, Floor, NON_TEXT, TEXT, lift_to_contrast, parse_hex, surface_for_background,
    to_hex,
};
pub use palette::{Appearance, CodePalette, ColorMode, Palette};

/// Background assumed for a light terminal that did not report one: an
/// off-white nearer the foreground than paper, so the floor errs toward
/// too dark.
const ASSUMED_LIGHT_BACKGROUND: &str = "#f5f5f5";

#[derive(Debug, Clone)]
pub struct Theme {
    pub mode: ColorMode,
    pub appearance: Appearance,
    pub palette: Palette,
    pub code: CodePalette,
}

impl Theme {
    /// The `system` theme, in the terminal's own colours.
    /// A dark terminal needs no contrast floors: its scheme already pairs
    /// its palette with its background. A light terminal's hex ramp is held
    /// to the floors against the reported background (an off-white when the
    /// terminal did not answer, so the floor errs toward too dark).
    pub fn system(mode: ColorMode, appearance: Appearance, background: Option<&str>) -> Self {
        let mut palette = palette::terminal_palette(mode, appearance);
        let mut code = palette::terminal_code_palette(mode, appearance);
        if palette::light_ramp_applies(mode, appearance)
            && let Some(bg) = parse_hex(background.unwrap_or(ASSUMED_LIGHT_BACKGROUND))
        {
            palette = with_floors(palette, bg);
            code = with_code_floor(code, bg);
        }
        Self {
            mode,
            appearance,
            palette,
            code,
        }
    }

    /// A registry theme on a terminal of `mode`. `background` is what the
    /// terminal reported, else the theme's own window colour; an unknown id
    /// (or `system`) gives the system theme.
    pub fn named(
        id: &str,
        mode: ColorMode,
        appearance: Appearance,
        background: Option<&str>,
    ) -> Self {
        let Some(entry) = themes::THEMES.iter().find(|t| t.id == id) else {
            return Self::system(mode, appearance, background);
        };
        let usable = matches!(mode, ColorMode::TrueColor | ColorMode::Ansi256)
            && entry.dark == matches!(appearance, Appearance::Dark);
        if !usable {
            return Self::system(mode, appearance, background);
        }
        let background = background.unwrap_or(entry.background);
        let own = |h: &str| h.to_owned();
        let mut palette = Palette {
            accent: own(entry.accent),
            on_accent: own(entry.background),
            user: own(entry.accent),
            keybind: own(entry.func),
            activity: palette::terminal_palette(mode, appearance).activity,
            text: own(entry.text),
            text_soft: own(entry.text_soft),
            muted: own(entry.muted),
            faint: own(entry.faint),
            success: own(entry.success),
            warning: own(entry.warning),
            danger: own(entry.danger),
            highlight: own(entry.highlight),
            border: own(entry.border),
            surface: entry.surface.map(own),
            scroll_thumb: own(entry.faint),
            scroll_track: entry.surface.map_or_else(
                || palette::terminal_palette(mode, appearance).scroll_track,
                own,
            ),
        };
        let mut code = CodePalette {
            keyword: own(entry.keyword),
            string: own(entry.string),
            number: own(entry.number),
            comment: own(entry.comment),
            func: own(entry.func),
            type_: own(entry.type_),
            constant: own(entry.constant),
            operator: own(entry.operator),
            punctuation: own(entry.punctuation),
        };
        if let Some(bg) = parse_hex(background) {
            palette = with_floors(palette, bg);
            code = with_code_floor(code, bg);
        }
        Self {
            mode,
            appearance,
            palette,
            code,
        }
    }

    /// Detects the default theme id and palette from terminal capabilities
    /// and background. Apple Terminal without a background signal uses System.
    pub fn detect(env: &detect::Env, is_tty: bool, osc11: Option<&str>) -> (&'static str, Self) {
        let mode = detect::color_mode(env, is_tty);
        let (appearance, background) = detect::resolve_appearance(env, osc11);
        let apple_terminal = env
            .get("TERM_PROGRAM")
            .is_some_and(|v| v == "Apple_Terminal")
            || env.get("TERM").is_some_and(|v| v.starts_with("nsterm"));
        let unknown_apple_background = apple_terminal
            && background.is_none()
            && detect::appearance_from_override(env).is_none()
            && detect::appearance_from_colorfgbg(env).is_none();
        let id = if matches!(mode, ColorMode::TrueColor | ColorMode::Ansi256)
            && appearance == Appearance::Dark
            && !unknown_apple_background
        {
            "dim-dark"
        } else {
            "system"
        };
        (id, Self::named(id, mode, appearance, background.as_deref()))
    }
}

fn lift(
    hex: &str,
    background: contrast::Rgb,
    surface: Option<contrast::Rgb>,
    floor: Floor,
) -> String {
    let Some(color) = parse_hex(hex) else {
        return hex.to_owned();
    };
    let on_background = lift_to_contrast(color, background, floor);
    let result = match surface {
        Some(surface) => lift_to_contrast(on_background, surface, floor),
        None => on_background,
    };
    to_hex(result)
}

fn with_floors(palette: Palette, background: contrast::Rgb) -> Palette {
    let surface = palette
        .surface
        .as_deref()
        .and_then(parse_hex)
        .map(|s| surface_for_background(s, background));
    let lift_text = |hex: &str, floor| lift(hex, background, surface, floor);
    let lift_bg = |hex: &str, floor| lift(hex, background, None, floor);
    Palette {
        accent: lift_text(&palette.accent, COLORED_TEXT),
        on_accent: palette.on_accent,
        user: lift_text(&palette.user, COLORED_TEXT),
        keybind: lift_text(&palette.keybind, COLORED_TEXT),
        activity: lift_text(&palette.activity, COLORED_TEXT),
        text: lift_text(&palette.text, TEXT),
        text_soft: lift_text(&palette.text_soft, TEXT),
        muted: lift_text(&palette.muted, TEXT),
        faint: lift_bg(&palette.faint, NON_TEXT),
        success: lift_text(&palette.success, COLORED_TEXT),
        warning: lift_text(&palette.warning, COLORED_TEXT),
        danger: lift_text(&palette.danger, COLORED_TEXT),
        highlight: lift_text(&palette.highlight, COLORED_TEXT),
        border: lift_bg(&palette.border, NON_TEXT),
        surface: surface.map(to_hex).or(palette.surface),
        scroll_thumb: lift_bg(&palette.scroll_thumb, NON_TEXT),
        scroll_track: surface.map(to_hex).unwrap_or(palette.scroll_track),
    }
}

fn with_code_floor(code: CodePalette, background: contrast::Rgb) -> CodePalette {
    let l = |hex: &str| lift(hex, background, None, COLORED_TEXT);
    CodePalette {
        keyword: l(&code.keyword),
        string: l(&code.string),
        number: l(&code.number),
        comment: l(&code.comment),
        func: l(&code.func),
        type_: l(&code.type_),
        constant: l(&code.constant),
        operator: l(&code.operator),
        punctuation: l(&code.punctuation),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_theme_is_the_terminals_own_colours() {
        for mode in [ColorMode::TrueColor, ColorMode::Ansi256, ColorMode::Ansi16] {
            let theme = Theme::system(mode, Appearance::Dark, None);
            assert_eq!(theme.palette.accent, "ansi:3");
            assert_eq!(theme.palette.text, "default");
            assert_eq!(theme.palette.muted, "dim");
            assert_eq!(theme.palette.surface, None);
        }
    }

    #[test]
    fn a_light_terminal_takes_the_hex_ramp_for_its_colours() {
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Light, Some("#ffffff"));
        let white = parse_hex("#ffffff").unwrap();
        for role in [
            &theme.palette.accent,
            &theme.palette.highlight,
            &theme.palette.keybind,
            &theme.palette.activity,
            &theme.code.number,
        ] {
            let color = parse_hex(role).expect("hex role on a light terminal");
            assert!(contrast::contrast_ratio(color, white) >= 4.5, "{role}");
        }
        assert_eq!(theme.palette.text, "default");
        assert!(contrast::contrast_ratio(parse_hex(&theme.palette.muted).unwrap(), white) >= 4.5);
        assert_eq!(theme.palette.on_accent, "#ffffff");
        let few = Theme::system(ColorMode::Ansi16, Appearance::Light, None);
        assert_eq!(few.palette.accent, "ansi:3");
        assert_eq!(few.palette.activity, "ansi:12");
        for mode in [ColorMode::TrueColor, ColorMode::Ansi256] {
            for id in ["system", "dim-light", "monokai-light"] {
                for background in ["#ffffff", "#f5f5f5", "#d0d0d0"] {
                    let theme = Theme::named(id, mode, Appearance::Light, Some(background));
                    let blue = parse_hex(&theme.palette.activity).unwrap();
                    let background = parse_hex(background).unwrap();
                    assert!(contrast::contrast_ratio(blue, background) >= COLORED_TEXT.ratio);
                    assert!(contrast::apca_lightness_contrast(blue, background) >= COLORED_TEXT.lc);
                    assert!(
                        blue[2] > blue[1] && blue[1] > blue[0],
                        "activity stays blue"
                    );
                }
            }
        }
    }

    #[test]
    fn dark_palette_on_a_white_background_is_lifted_to_stay_readable() {
        let theme = Theme::named(
            "monokai-dark",
            ColorMode::TrueColor,
            Appearance::Dark,
            Some("#ffffff"),
        );
        let white = parse_hex("#ffffff").unwrap();
        let accent = parse_hex(&theme.palette.accent).unwrap();
        assert!(contrast::contrast_ratio(accent, white) >= 4.5);
        let activity = parse_hex(&theme.palette.activity).unwrap();
        assert!(contrast::contrast_ratio(activity, white) >= COLORED_TEXT.ratio);
    }
}
