//! SGR helpers for plain-string rows.
//!
//! A colour is a spec string, the one form every theme role takes:
//! - `#rrggbb`: that exact colour (truecolor escape; the palettes are
//!   already snapped to what a lower-depth terminal shows),
//! - `ansi:N`: slot `N` of the terminal's own palette, so the role follows
//!   the user's colour scheme (as Amp's default theme does),
//! - `default`: the terminal's default foreground,
//! - `dim`: the default foreground with the faint attribute (foreground only).

#[derive(Default, Clone, Copy)]
pub struct Paint<'a> {
    pub fg: Option<&'a str>,
    pub bg: Option<&'a str>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
}

/// What a colour spec asks the terminal for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spec {
    Rgb(u8, u8, u8),
    Slot(u8),
    Default,
    Dim,
}

pub fn spec(color: &str) -> Option<Spec> {
    match color {
        "default" => return Some(Spec::Default),
        "dim" => return Some(Spec::Dim),
        _ => {}
    }
    if let Some(slot) = color.strip_prefix("ansi:") {
        return slot.parse().ok().map(Spec::Slot);
    }
    let value = color.strip_prefix('#')?;
    if value.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&value[0..2], 16).ok()?;
    let g = u8::from_str_radix(&value[2..4], 16).ok()?;
    let b = u8::from_str_radix(&value[4..6], 16).ok()?;
    Some(Spec::Rgb(r, g, b))
}

/// The SGR parameters for `color`. Slots 0-15 use the 16-colour codes every
/// terminal understands.
fn color_code(color: &str, background: bool) -> Option<String> {
    let base = if background { 40 } else { 30 };
    Some(match spec(color)? {
        Spec::Rgb(r, g, b) => format!("{};2;{r};{g};{b}", base + 8),
        Spec::Slot(n @ 0..=7) => format!("{}", base + n as u32),
        Spec::Slot(n @ 8..=15) => format!("{}", base + 60 + n as u32 - 8),
        Spec::Slot(n) => format!("{};5;{n}", base + 8),
        Spec::Default => return None,
        Spec::Dim if background => return None,
        Spec::Dim => "2".to_owned(),
    })
}

/// `color` for cell-based renderers (Markdown, the composer); `dim` has no
/// colour of its own and reads as the default foreground there.
pub fn tui_color(color: &str) -> Option<ratatui::style::Color> {
    use ratatui::style::Color;
    Some(match spec(color)? {
        Spec::Rgb(r, g, b) => Color::Rgb(r, g, b),
        Spec::Slot(n) => Color::Indexed(n),
        Spec::Default | Spec::Dim => Color::Reset,
    })
}

/// `text` styled with `paint`; empty text and an empty paint pass through.
/// Codes are written bold, dim, italic, underline, then colours.
pub fn paint(text: &str, paint: Paint<'_>) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut codes: Vec<String> = Vec::new();
    if paint.bold {
        codes.push("1".into());
    }
    if paint.dim {
        codes.push("2".into());
    }
    if paint.italic {
        codes.push("3".into());
    }
    if paint.underline {
        codes.push("4".into());
    }
    if let Some(code) = paint.fg.and_then(|c| color_code(c, false)) {
        codes.push(code);
    }
    if let Some(code) = paint.bg.and_then(|c| color_code(c, true)) {
        codes.push(code);
    }
    if codes.is_empty() {
        return text.to_owned();
    }
    format!("\x1b[{}m{text}\x1b[0m", codes.join(";"))
}

pub fn fg(text: &str, color: &str) -> String {
    paint(
        text,
        Paint {
            fg: Some(color),
            ..Paint::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_are_truecolor_and_runs_are_closed() {
        assert_eq!(fg("x", "#a9dc76"), "\x1b[38;2;169;220;118mx\x1b[0m");
        assert_eq!(
            paint(
                "x",
                Paint {
                    bold: true,
                    fg: Some("#000000"),
                    bg: Some("#ffffff"),
                    ..Paint::default()
                }
            ),
            "\x1b[1;38;2;0;0;0;48;2;255;255;255mx\x1b[0m"
        );
        assert_eq!(
            paint(
                "",
                Paint {
                    bold: true,
                    ..Paint::default()
                }
            ),
            ""
        );
        assert_eq!(paint("x", Paint::default()), "x");
    }

    #[test]
    fn terminal_specs_follow_the_terminal_palette() {
        assert_eq!(fg("x", "ansi:3"), "\x1b[33mx\x1b[0m");
        assert_eq!(fg("x", "ansi:8"), "\x1b[90mx\x1b[0m");
        assert_eq!(fg("x", "ansi:243"), "\x1b[38;5;243mx\x1b[0m");
        assert_eq!(fg("x", "dim"), "\x1b[2mx\x1b[0m");
        assert_eq!(fg("x", "default"), "x");
        let selected = Paint {
            fg: Some("ansi:0"),
            bg: Some("ansi:3"),
            ..Paint::default()
        };
        assert_eq!(paint("x", selected), "\x1b[30;43mx\x1b[0m");
    }
}
