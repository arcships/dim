//! The scrollback palettes (port of `palette.ts`): one role set per terminal
//! colour mode and appearance. The app paints no background of its own except
//! the user-message band, so every role is written for one side of the
//! terminal background.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    TrueColor,
    Ansi256,
    Ansi16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub accent: String,
    pub text: String,
    pub text_soft: String,
    pub muted: String,
    pub faint: String,
    pub success: String,
    pub warning: String,
    pub danger: String,
    pub highlight: String,
    pub border: String,
    /// The user-message band; none where a terminal has no subtle step.
    pub surface: Option<String>,
}

/// Syntax colours for fenced code, kept apart from the chrome roles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodePalette {
    pub keyword: String,
    pub string: String,
    pub number: String,
    pub comment: String,
    pub func: String,
    pub type_: String,
    pub constant: String,
    pub operator: String,
    pub punctuation: String,
}

#[allow(clippy::too_many_arguments)]
fn pal(
    accent: &str,
    text: &str,
    text_soft: &str,
    muted: &str,
    faint: &str,
    success: &str,
    warning: &str,
    danger: &str,
    highlight: &str,
    border: &str,
    surface: Option<&str>,
) -> Palette {
    Palette {
        accent: accent.into(),
        text: text.into(),
        text_soft: text_soft.into(),
        muted: muted.into(),
        faint: faint.into(),
        success: success.into(),
        warning: warning.into(),
        danger: danger.into(),
        highlight: highlight.into(),
        border: border.into(),
        surface: surface.map(str::to_owned),
    }
}

pub fn palette_for(mode: ColorMode, appearance: Appearance) -> Palette {
    use Appearance::{Dark, Light};
    use ColorMode::{Ansi16, Ansi256, TrueColor};
    match (mode, appearance) {
        (TrueColor, Dark) => pal(
            "#a9dc76",
            "#e4e4e7",
            "#d4d4d8",
            "#a1a1aa",
            "#85858f",
            "#a9dc76",
            "#e5c07b",
            "#e06c75",
            "#d19a66",
            "#52525b",
            Some("#2a2a31"),
        ),
        (TrueColor, Light) => pal(
            "#15803d",
            "#18181b",
            "#3f3f46",
            "#5b5b64",
            "#82828e",
            "#4d7c0f",
            "#a16207",
            "#b91c1c",
            "#c2410c",
            "#c9c9d1",
            Some("#eaeaee"),
        ),
        (Ansi256, Dark) => pal(
            "#afd787",
            "#eeeeee",
            "#d7d7d7",
            "#a8a8a8",
            "#8a8a8a",
            "#afd787",
            "#ffd787",
            "#ff8787",
            "#ffaf87",
            "#4e4e4e",
            Some("#303030"),
        ),
        (Ansi256, Light) => pal(
            "#008700",
            "#262626",
            "#444444",
            "#585858",
            "#878787",
            "#5f8700",
            "#af5f00",
            "#af0000",
            "#d75f00",
            "#d0d0d0",
            Some("#e4e4e4"),
        ),
        (Ansi16, Dark) => pal(
            "#55ff55", "#c0c0c0", "#c0c0c0", "#808080", "#808080", "#00aa00", "#aaaa00", "#aa0000",
            "#aa5500", "#808080", None,
        ),
        (Ansi16, Light) => pal(
            "#00aa00", "#000000", "#000000", "#808080", "#808080", "#00aa00", "#aa5500", "#aa0000",
            "#aa5500", "#808080", None,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn code(
    keyword: &str,
    string: &str,
    number: &str,
    comment: &str,
    func: &str,
    type_: &str,
    constant: &str,
    operator: &str,
    punctuation: &str,
) -> CodePalette {
    CodePalette {
        keyword: keyword.into(),
        string: string.into(),
        number: number.into(),
        comment: comment.into(),
        func: func.into(),
        type_: type_.into(),
        constant: constant.into(),
        operator: operator.into(),
        punctuation: punctuation.into(),
    }
}

pub fn code_palette_for(mode: ColorMode, appearance: Appearance) -> CodePalette {
    use Appearance::{Dark, Light};
    use ColorMode::{Ansi16, Ansi256, TrueColor};
    match (mode, appearance) {
        (TrueColor, Dark) => code(
            "#c678dd", "#98c379", "#d19a66", "#7d8590", "#61afef", "#e5c07b", "#d19a66", "#56b6c2",
            "#8b949e",
        ),
        (TrueColor, Light) => code(
            "#8250df", "#1a7f37", "#b45309", "#6e7781", "#0550ae", "#953800", "#b45309", "#0e7490",
            "#57606a",
        ),
        (Ansi256, Dark) => code(
            "#d787d7", "#afd787", "#ffaf87", "#808080", "#87afff", "#ffd787", "#ffaf87", "#87d7d7",
            "#949494",
        ),
        (Ansi256, Light) => code(
            "#8700af", "#008700", "#af5f00", "#6c6c6c", "#0000af", "#875f00", "#af5f00", "#008787",
            "#585858",
        ),
        (Ansi16, Dark) => code(
            "#aa00aa", "#00aa00", "#aa5500", "#808080", "#0000aa", "#aaaa00", "#aa5500", "#00aaaa",
            "#808080",
        ),
        (Ansi16, Light) => code(
            "#aa00aa", "#00aa00", "#aa5500", "#808080", "#0000aa", "#aa5500", "#aa5500", "#00aaaa",
            "#000000",
        ),
    }
}
