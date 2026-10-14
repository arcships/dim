//! Bordered panels shared by the palette, the queue strip and the prompts
//! (ports of `renderBorderedBlock`, `panelChoices` and `ConfirmPanel`).

use crate::ui::theme::Palette;
use crate::ui::theme::ansi::{Paint, fg, paint};
use crate::ui::transcript::blocks::{display_width, truncate_cells};

pub const SELECTED_MARKER: &str = "› ";
pub const IDLE_MARKER: &str = "  ";

/// Display width ignoring SGR/OSC sequences.
pub fn visible_width(text: &str) -> usize {
    let mut plain = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for n in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(n) = chars.next() {
                        if n == '\x07' {
                            break;
                        }
                        if n == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        plain.push(c);
    }
    display_width(&plain)
}

/// Borders in `color`, one column of padding, rows padded with blanks to
/// `height` when given. `title` sits in the top border (`╭─ Queue (1) ──╮`).
pub fn bordered(
    content: &[String],
    width: usize,
    height: Option<usize>,
    color: &str,
    title: Option<&str>,
) -> Vec<String> {
    let inner = width.saturating_sub(2);
    let content_width = inner.saturating_sub(2);
    let edge = |s: &str| fg(s, color);
    let top = match title {
        Some(title) if inner > display_width(title) + 3 => {
            let fill = inner - display_width(title) - 3;
            format!("╭─ {title} {}╮", "─".repeat(fill))
        }
        _ => format!("╭{}╮", "─".repeat(inner)),
    };
    let mut out = vec![edge(&top)];
    let body_rows = height.map(|h| h.saturating_sub(2));
    for line in content.iter().take(body_rows.unwrap_or(usize::MAX)) {
        let used = visible_width(line);
        out.push(format!(
            "{} {line}{} {}",
            edge("│"),
            " ".repeat(content_width.saturating_sub(used)),
            edge("│")
        ));
    }
    if let Some(rows) = body_rows {
        while out.len() < rows + 1 {
            out.push(format!("{}{}{}", edge("│"), " ".repeat(inner), edge("│")));
        }
    }
    out.push(edge(&format!("╰{}╯", "─".repeat(inner))));
    out
}

/// Horizontal choice row: the focused option carries the accent marker.
pub fn choices(labels: &[String], focused: usize, p: &Palette) -> String {
    labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            if index == focused {
                paint(
                    &format!("{SELECTED_MARKER}{label}"),
                    Paint {
                        fg: Some(&p.accent),
                        bold: true,
                        ..Paint::default()
                    },
                )
            } else {
                fg(&format!("{IDLE_MARKER}{label}"), &p.text_soft)
            }
        })
        .collect::<Vec<_>>()
        .join("   ")
}

/// A warning-titled confirmation: title, optional body, choices, hint.
pub fn confirm_panel(
    title: &str,
    body: &[String],
    options: &[String],
    focused: usize,
    hint: &str,
    width: usize,
    p: &Palette,
) -> Vec<String> {
    let inner = width.saturating_sub(4).max(1);
    let mut content = vec![paint(
        &truncate_cells(title, inner),
        Paint {
            fg: Some(&p.warning),
            bold: true,
            ..Paint::default()
        },
    )];
    content.push(String::new());
    for line in body {
        content.push(fg(&truncate_cells(line, inner), &p.text));
    }
    if !body.is_empty() {
        content.push(String::new());
    }
    content.push(choices(options, focused, p));
    content.push(String::new());
    content.push(fg(&truncate_cells(hint, inner), &p.muted));
    bordered(&content, width, None, &p.border, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{Appearance, ColorMode, Theme};

    fn plain(s: &str) -> String {
        let mut out = String::new();
        let mut skip = false;
        for c in s.chars() {
            match (skip, c) {
                (false, '\x1b') => skip = true,
                (true, 'm') => skip = false,
                (false, c) => out.push(c),
                _ => {}
            }
        }
        out
    }

    #[test]
    fn titled_border_and_fixed_height() {
        let rows = bordered(&["a".into()], 30, Some(5), "#555555", Some("Queue (1)"));
        let text: Vec<String> = rows.iter().map(|r| plain(r)).collect();
        assert_eq!(text.len(), 5);
        assert!(text[0].starts_with("╭─ Queue (1) ─"));
        assert_eq!(display_width(&text[0]), 30);
        assert!(text.iter().all(|r| display_width(r) == 30));
    }

    #[test]
    fn confirm_panel_layout() {
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Dark, None);
        let panel = confirm_panel(
            "Exit dimcode?",
            &["The agent is still running.".into()],
            &["Exit".into(), "Stay".into()],
            1,
            "y exit · n stay",
            60,
            &theme.palette,
        );
        let text: Vec<String> = panel.iter().map(|r| plain(r)).collect();
        assert!(text[1].contains("Exit dimcode?"));
        assert!(
            text.iter()
                .any(|r| r.contains("  Exit") && r.contains("› Stay"))
        );
    }
}
