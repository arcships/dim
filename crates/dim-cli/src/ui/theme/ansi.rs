//! SGR helpers for plain-string rows. Truecolor escapes are always written,
//! whatever the terminal's depth: the palettes are already snapped to the
//! entries a lower-depth terminal can show, and the terminal does the rest.

#[derive(Default, Clone, Copy)]
pub struct Paint<'a> {
    pub fg: Option<&'a str>,
    pub bg: Option<&'a str>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
}

fn rgb_code(hex: &str, background: bool) -> Option<String> {
    let value = hex.strip_prefix('#')?;
    if value.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&value[0..2], 16).ok()?;
    let g = u8::from_str_radix(&value[2..4], 16).ok()?;
    let b = u8::from_str_radix(&value[4..6], 16).ok()?;
    Some(format!(
        "{};2;{r};{g};{b}",
        if background { 48 } else { 38 }
    ))
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
    if let Some(code) = paint.fg.and_then(|c| rgb_code(c, false)) {
        codes.push(code);
    }
    if let Some(code) = paint.bg.and_then(|c| rgb_code(c, true)) {
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
}
