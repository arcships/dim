//! The window title: the product, the session's topic, and a mark for the run.
//!
//! The title reads `DIM | topic`, the topic being the session's first message
//! cut to a glance, or `DIM` alone before there is one. While a run works, a
//! quarter-circle turns in front of it; while one waits on a permission or a
//! question, a still dot stands there instead. Tab bars show the title where
//! no progress bar (`progress`) or status record (`status`) is drawn.
//!
//! Apple Terminal keeps no title stack and is set with OSC 2; elsewhere OSC 0
//! sets both window and icon title over the shell's, which entering the
//! terminal pushed (`terminal::modes::Mode::Title`).

use std::time::Instant;

use crate::ui::status::Status;

/// The product's name, the whole title before the session has a topic.
pub const BASE: &str = "DIM";
/// Longest topic in characters, the ellipsis included.
const TOPIC_MAX: usize = 20;
/// The run's mark while it works, a quarter turn clockwise each frame.
const SPINNER: [char; 4] = ['◐', '◓', '◑', '◒'];
/// How long each spinner frame shows.
const SPINNER_FRAME_MS: u128 = 120;
/// The run's mark while it waits on the user.
const WAITING: char = '●';

/// Whether the terminal is Apple Terminal, which has no title stack.
pub fn apple_terminal() -> bool {
    let term = std::env::var("TERM").unwrap_or_default();
    std::env::var("TERM_PROGRAM").is_ok_and(|v| v == "Apple_Terminal") || term.starts_with("nsterm")
}

/// The sequence that sets the window title to `text`.
pub fn sequence(text: &str, apple: bool) -> Vec<u8> {
    let osc = if apple { 2 } else { 0 };
    format!("\x1b]{osc};{text}\x07").into_bytes()
}

/// A topic from a message: one line without control characters, cut to
/// [`TOPIC_MAX`] characters with an ellipsis.
pub fn topic(message: &str) -> Option<String> {
    let line = message
        .split_whitespace()
        .map(|word| word.chars().filter(|c| !c.is_control()).collect::<String>())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if line.is_empty() {
        return None;
    }
    if line.chars().count() <= TOPIC_MAX {
        return Some(line);
    }
    let mut cut: String = line.chars().take(TOPIC_MAX - 1).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    Some(cut)
}

/// The title for `status` and `topic`, `elapsed` into the run.
fn text(status: &Status, topic: Option<&str>, elapsed_ms: u128) -> String {
    let mut title = String::new();
    match status {
        Status::Working => {
            let frame = (elapsed_ms / SPINNER_FRAME_MS) as usize % SPINNER.len();
            title.push(SPINNER[frame]);
            title.push(' ');
        }
        Status::Permission(_) | Status::Question => {
            title.push(WAITING);
            title.push(' ');
        }
        Status::Idle | Status::Done | Status::Error => {}
    }
    title.push_str(BASE);
    if let Some(topic) = topic {
        title.push_str(" | ");
        title.push_str(topic);
    }
    title
}

/// Sets the title as the run and the topic change, and turns the spinner.
#[derive(Debug)]
pub struct WindowTitle {
    apple: bool,
    /// When the spinner started turning; the frame follows from it.
    started: Instant,
    /// The title the terminal was last given; `None` once it was handed back.
    shown: Option<String>,
}

impl WindowTitle {
    pub fn new(apple: bool, now: Instant) -> Self {
        Self {
            apple,
            started: now,
            shown: Some(BASE.to_owned()),
        }
    }

    /// The sequence that brings the title to `status` and `topic` at `now`,
    /// unless the terminal shows it already.
    pub fn report(
        &mut self,
        status: &Status,
        topic: Option<&str>,
        now: Instant,
    ) -> Option<Vec<u8>> {
        if *status != Status::Working {
            self.started = now;
        }
        let title = text(status, topic, now.duration_since(self.started).as_millis());
        if self.shown.as_deref() == Some(title.as_str()) {
            return None;
        }
        let bytes = sequence(&title, self.apple);
        self.shown = Some(title);
        Some(bytes)
    }

    /// The terminal was handed back and entered again, which set the title
    /// to [`BASE`]: the next report sets it again.
    pub fn forget(&mut self) {
        self.shown = None;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn the_title_names_the_product_and_the_topic() {
        assert_eq!(text(&Status::Idle, None, 0), "DIM");
        assert_eq!(text(&Status::Done, Some("fix login"), 0), "DIM | fix login");
        assert_eq!(
            text(&Status::Question, Some("fix login"), 0),
            "● DIM | fix login"
        );
        assert_eq!(text(&Status::Permission("exec".into()), None, 0), "● DIM");
    }

    #[test]
    fn a_working_run_turns_the_spinner_once_a_frame() {
        let frames: Vec<String> = (0..5)
            .map(|i| text(&Status::Working, None, i * SPINNER_FRAME_MS))
            .collect();
        assert_eq!(frames, ["◐ DIM", "◓ DIM", "◑ DIM", "◒ DIM", "◐ DIM"]);
    }

    #[test]
    fn a_topic_is_one_short_line() {
        assert_eq!(
            topic("  fix\nthe\tlogin  ").as_deref(),
            Some("fix the login")
        );
        assert_eq!(topic(" \n "), None);
        assert_eq!(topic("\x1b]0;x\x07ok").as_deref(), Some("]0;xok"));
        assert_eq!(
            topic("refactor the session host into ports").as_deref(),
            Some("refactor the sessio…")
        );
        assert_eq!(
            topic("把标题改成会话的第一条消息并加上一个转圈的加载动画").as_deref(),
            Some("把标题改成会话的第一条消息并加上一个转…")
        );
    }

    #[test]
    fn the_title_is_set_only_when_it_changes() {
        let start = Instant::now();
        let mut title = WindowTitle::new(false, start);
        assert_eq!(title.report(&Status::Idle, None, start), None);
        assert_eq!(
            title.report(&Status::Working, Some("hi"), start),
            Some(b"\x1b]0;\xe2\x97\x90 DIM | hi\x07".to_vec())
        );
        let later = start + Duration::from_millis(50);
        assert_eq!(title.report(&Status::Working, Some("hi"), later), None);
        let next = start + Duration::from_millis(130);
        assert_eq!(
            title.report(&Status::Working, Some("hi"), next),
            Some("\x1b]0;◓ DIM | hi\x07".as_bytes().to_vec())
        );
        title.forget();
        assert_eq!(
            title.report(&Status::Idle, Some("hi"), next),
            Some(b"\x1b]0;DIM | hi\x07".to_vec())
        );
        let mut apple = WindowTitle::new(true, start);
        assert_eq!(
            apple.report(&Status::Idle, Some("hi"), start),
            Some(b"\x1b]2;DIM | hi\x07".to_vec())
        );
    }
}
