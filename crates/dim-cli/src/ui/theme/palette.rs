//! Colour roles and the System theme. Every role is a colour spec (see
//! [`super::ansi`]). System uses the terminal's own foreground,
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
    /// Model activity, blue in every theme.
    pub activity: String,
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

/// Syntax colours for fenced code, kept apart from the chrome roles. A
/// colour written as a palette slot (`ansi:N`) follows the terminal's scheme,
/// so the highlighter must use its polarity-safe syntax mode; hex colours are
/// used as they are.
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

/// The chromatic roles of a light terminal, as the TS build painted them:
/// the 16-colour slots of a light profile (Terminal.app's ochre yellow and
/// sea-green cyan on white among them) were not chosen for reading on their
/// own background, so each hue drops to the lightness that carries it on
/// paper. The faint attribute is a light grey on Terminal.app's white, so
/// muted and faint take the TS build's greys too (`L 0.47` / `0.61`);
/// text and border stay the terminal's own, which it pairs with its
/// background.
pub struct LightRamp {
    pub yellow: &'static str,
    pub green: &'static str,
    pub blue: &'static str,
    pub cyan: &'static str,
    pub red: &'static str,
    pub muted: &'static str,
    pub faint: &'static str,
}

pub const LIGHT_RAMP: LightRamp = LightRamp {
    yellow: "#854d0e",
    green: "#3f6212",
    blue: "#1e40af",
    cyan: "#155e75",
    red: "#991b1b",
    muted: "#5b5b64",
    faint: "#82828e",
};

/// True where a light terminal takes the hex ramp: 16 colours have no other
/// yellow to offer.
pub fn light_ramp_applies(mode: ColorMode, appearance: Appearance) -> bool {
    appearance == Appearance::Light && mode != ColorMode::Ansi16
}

/// The default theme. The scrollbar takes fixed greys of the 256-colour
/// cube, one step off the background of each appearance.
pub fn terminal_palette(mode: ColorMode, appearance: Appearance) -> Palette {
    let own = |spec: &str| spec.to_owned();
    let (thumb, track) = match appearance {
        Appearance::Dark => ("ansi:243", "ansi:236"),
        Appearance::Light => ("ansi:248", "ansi:254"),
    };
    let light = light_ramp_applies(mode, appearance);
    let slot = |ansi: &str, ramp: &'static str| own(if light { ramp } else { ansi });
    Palette {
        accent: slot("ansi:3", LIGHT_RAMP.yellow),
        on_accent: own(if light { "#ffffff" } else { "ansi:0" }),
        user: slot("ansi:2", LIGHT_RAMP.green),
        keybind: slot("ansi:4", LIGHT_RAMP.blue),
        activity: own(match (mode, appearance) {
            (ColorMode::Ansi16, _) => "ansi:12",
            (_, Appearance::Light) => "#0369a1",
            (ColorMode::TrueColor, Appearance::Dark) => "#38bdf8",
            (ColorMode::Ansi256, Appearance::Dark) => "ansi:75",
        }),
        text: own("default"),
        text_soft: own("default"),
        muted: slot("dim", LIGHT_RAMP.muted),
        faint: slot("ansi:8", LIGHT_RAMP.faint),
        success: slot("ansi:2", LIGHT_RAMP.green),
        warning: slot("ansi:3", LIGHT_RAMP.yellow),
        danger: slot("ansi:1", LIGHT_RAMP.red),
        highlight: slot("ansi:6", LIGHT_RAMP.cyan),
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

/// Syntax colours of the default theme, as palette slots: keywords
/// and types blue, calls cyan, numbers yellow, strings green, operators
/// plain. A light terminal takes the same hues from the ramp.
pub fn terminal_code_palette(mode: ColorMode, appearance: Appearance) -> CodePalette {
    if light_ramp_applies(mode, appearance) {
        let r = &LIGHT_RAMP;
        return code(
            r.blue, r.green, r.yellow, r.muted, r.cyan, r.blue, r.yellow, "default", "default",
        );
    }
    code(
        "ansi:4", "ansi:2", "ansi:3", "ansi:8", "ansi:6", "ansi:4", "ansi:3", "default", "default",
    )
}
