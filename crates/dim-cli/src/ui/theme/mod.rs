//! Colour roles of the terminal interface: the default theme (Amp's,
//! following the terminal's own colours), the named themes with contrast
//! floors against the real background, detection.

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

#[derive(Debug, Clone)]
pub struct Theme {
    pub mode: ColorMode,
    pub appearance: Appearance,
    pub palette: Palette,
    pub code: CodePalette,
}

impl Theme {
    /// The `system` theme: Amp's default, in the terminal's own colours.
    /// It needs no contrast floors: the terminal's scheme already pairs its
    /// palette with its background.
    pub fn system(mode: ColorMode, appearance: Appearance) -> Self {
        Self {
            mode,
            appearance,
            palette: palette::terminal_palette(appearance),
            code: palette::terminal_code_palette(),
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
            return Self::system(mode, appearance);
        };
        // A dark theme on a light terminal (or the reverse) would be
        // unreadable, and 16/8-colour terminals only get `system`.
        let usable = matches!(mode, ColorMode::TrueColor | ColorMode::Ansi256)
            && entry.dark == matches!(appearance, Appearance::Dark);
        if !usable {
            return Self::system(mode, appearance);
        }
        let background = background.unwrap_or(entry.background);
        let own = |h: &str| h.to_owned();
        // The roles Amp's structure adds come from the theme's own: users
        // speak in its accent, and the accent bar carries its background.
        let mut palette = Palette {
            accent: own(entry.accent),
            on_accent: own(entry.background),
            user: own(entry.accent),
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
            scroll_track: entry
                .surface
                .map_or_else(|| palette::terminal_palette(appearance).scroll_track, own),
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

    /// Detects mode and appearance from the environment (and an OSC 11
    /// report, when the terminal answered one).
    pub fn detect(env: &detect::Env, is_tty: bool, osc11: Option<&str>) -> Self {
        let mode = detect::color_mode(env, is_tty);
        let (appearance, _) = detect::resolve_appearance(env, osc11);
        Self::system(mode, appearance)
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
    fn the_default_theme_is_the_terminals_own_colours() {
        for mode in [ColorMode::TrueColor, ColorMode::Ansi256, ColorMode::Ansi16] {
            let theme = Theme::system(mode, Appearance::Dark);
            assert_eq!(theme.palette.accent, "ansi:3");
            assert_eq!(theme.palette.text, "default");
            assert_eq!(theme.palette.muted, "dim");
            assert_eq!(theme.palette.surface, None);
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
    }
}
