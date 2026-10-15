//! Colour roles and the default theme. Every role is a colour spec (see
//! [`super::ansi`]). The default is Amp's: the terminal's own foreground,
//! slots of its palette and the faint attribute, so it follows whatever
//! colour scheme the terminal has, on dark and light backgrounds alike.

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
    /// Titles, the selected row's background, inline code.
    pub accent: String,
    /// Text on an accent background.
    pub on_accent: String,
    /// The user's messages.
    pub user: String,
    /// Keys named in menus and hints.
    pub keybind: String,
    pub text: String,
    pub text_soft: String,
    pub muted: String,
    pub faint: String,
    pub success: String,
    pub warning: String,
    pub danger: String,
    /// Headings and the transient notice.
    pub highlight: String,
    pub border: String,
    /// The user-message band; none where a terminal has no subtle step.
    pub surface: Option<String>,
    pub scroll_thumb: String,
    pub scroll_track: String,
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

/// Amp's default theme. The scrollbar takes fixed greys of the 256-colour
/// cube, as Amp does, one step off the background of each appearance.
pub fn terminal_palette(appearance: Appearance) -> Palette {
    let own = |spec: &str| spec.to_owned();
    let (thumb, track) = match appearance {
        Appearance::Dark => ("ansi:243", "ansi:236"),
        Appearance::Light => ("ansi:248", "ansi:254"),
    };
    Palette {
        accent: own("ansi:3"),
        on_accent: own("ansi:0"),
        user: own("ansi:2"),
        keybind: own("ansi:4"),
        text: own("default"),
        text_soft: own("default"),
        muted: own("dim"),
        faint: own("ansi:8"),
        success: own("ansi:2"),
        warning: own("ansi:3"),
        danger: own("ansi:1"),
        highlight: own("ansi:6"),
        border: own("default"),
        surface: None,
        scroll_thumb: own(thumb),
        scroll_track: own(track),
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

/// Syntax colours of the default theme, as palette slots.
pub fn terminal_code_palette() -> CodePalette {
    code(
        "ansi:5", "ansi:2", "ansi:3", "ansi:8", "ansi:4", "ansi:3", "ansi:3", "ansi:6", "default",
    )
}
