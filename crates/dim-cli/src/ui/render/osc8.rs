//! OSC 8 hyperlinks. Both the link target and the visible text are stripped
//! of control characters first: an unsanitized target could end the sequence
//! early and inject arbitrary terminal commands.

/// Longest target accepted; terminals commonly cap URIs around 2 KiB.
const MAX_TARGET: usize = 2048;

fn sanitize(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// `text` as a hyperlink to `target`, or plain sanitized text when the target
/// is empty or too long after sanitizing.
pub fn hyperlink(target: &str, text: &str) -> String {
    let target = sanitize(target);
    let text = sanitize(text);
    if target.is_empty() || target.len() > MAX_TARGET {
        return text;
    }
    format!("\x1b]8;;{target}\x1b\\{text}\x1b]8;;\x1b\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_text_in_open_and_close() {
        assert_eq!(
            hyperlink("https://a.example/x", "docs"),
            "\x1b]8;;https://a.example/x\x1b\\docs\x1b]8;;\x1b\\"
        );
    }

    #[test]
    fn control_characters_cannot_end_the_sequence() {
        let out = hyperlink("https://a\x07\x1b]0;pwned\x07.example", "go\x1b[31m");
        assert!(!out.contains('\x07'));
        assert_eq!(out.matches('\x1b').count(), 4, "only the two OSC 8 frames");
        assert!(out.contains("go[31m"));
    }

    #[test]
    fn empty_or_oversized_target_degrades_to_text() {
        assert_eq!(hyperlink("", "plain"), "plain");
        assert_eq!(hyperlink(&"a".repeat(MAX_TARGET + 1), "plain"), "plain");
    }
}
