//! Text from the model, tools or the App Server made safe to print: escape
//! sequences and other controls are removed, so they cannot move the cursor
//! or rewrite the terminal.

pub fn safe(text: &str) -> String {
    let mut output = String::new();
    let mut escape = 0;
    for ch in text.chars() {
        match escape {
            1 => {
                escape = match ch {
                    '[' => 2,
                    ']' | 'P' | '_' | '^' => 3,
                    _ => 0,
                }
            }
            2 => {
                if ('@'..='~').contains(&ch) {
                    escape = 0;
                }
            }
            3 => {
                if ch == '\u{7}' || ch == '\u{9c}' {
                    escape = 0;
                } else if ch == '\u{1b}' {
                    escape = 4;
                }
            }
            4 => escape = if ch == '\\' { 0 } else { 3 },
            _ => match ch {
                '\u{1b}' => escape = 1,
                '\u{9b}' => escape = 2,
                '\u{9d}' => escape = 3,
                '\n' | '\t' => output.push(ch),
                ch if !ch.is_control()
                    && !matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') =>
                {
                    output.push(ch)
                }
                _ => {}
            },
        }
    }
    output
}

/// [`safe`] for text that is drawn on one line: every newline, carriage
/// return and tab becomes a space, so a name cannot break a row, a heading,
/// a title or a column into pieces.
pub fn safe_line(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    for ch in text.chars() {
        folded.push(match ch {
            '\n' | '\r' | '\t' => ' ',
            ch => ch,
        });
    }
    safe(&folded)
}
