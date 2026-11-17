//! The window title: the product, the session's topic, and a mark for the run.
//!
//! The title reads `DIM | topic`, the topic being the Session's stored title
//! when it has one and otherwise its first message, cut to a glance, or `DIM`
//! alone before either. While a run works, a quarter-circle turns in front of
//! it; while one waits on a permission or a question, a still dot stands there
//! instead. Tab bars show the title where no progress bar (`progress`) or
//! status record (`status`) is drawn.
//!
//! A terminal that answered the startup probe's program status query keeps
//! the record and draws its own indicator from it, so the title yields: it
//! writes the plain text, with no mark and no frame to turn. `report` then
//! returns nothing once the text is set. `DIM_TERMINAL_TITLE=always` keeps the
//! mark for a terminal that answers but shows nothing.
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
/// The environment switch: `always` keeps the mark for a terminal that
/// answers the program status query but shows nothing.
const ENV_KEY: &str = "DIM_TERMINAL_TITLE";

/// Whether the title keeps its mark while a run works: always, except where
/// the terminal `program_status` names keeps the record and
/// `DIM_TERMINAL_TITLE` does not say `always`.
fn marks(program_status: bool) -> bool {
    let forced = std::env::var(ENV_KEY).is_ok_and(|v| v.trim().eq_ignore_ascii_case("always"));
    !program_status || forced
}

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

/// The title for `status` and `topic`, `elapsed` into the run; `mark` off
/// drops the run's own mark, since the terminal draws one from the record.
fn text(status: &Status, topic: Option<&str>, elapsed_ms: u128, mark: bool) -> String {
    let mut title = String::new();
    if mark {
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
    /// The terminal keeps the record, so the title text carries no mark and
    /// never changes frame to frame.
    mark: bool,
    /// When the spinner started turning; the frame follows from it.
    started: Instant,
    /// The title the terminal was last given; `None` once it was handed back.
    shown: Option<String>,
}

impl WindowTitle {
    /// The title of this process, which keeps its mark unless
    /// `program_status` says the terminal keeps the record instead.
    pub fn from_env(program_status: bool, now: Instant) -> Self {
        Self::new(apple_terminal(), marks(program_status), now)
    }

    fn new(apple: bool, mark: bool, now: Instant) -> Self {
        Self {
            apple,
            mark,
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
        let title = text(
            status,
            topic,
            now.duration_since(self.started).as_millis(),
            self.mark,
        );
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
        assert_eq!(text(&Status::Idle, None, 0, true), "DIM");
        assert_eq!(
            text(&Status::Done, Some("fix login"), 0, true),
            "DIM | fix login"
        );
        assert_eq!(
            text(&Status::Question, Some("fix login"), 0, true),
            "● DIM | fix login"
        );
        assert_eq!(
            text(&Status::Permission("exec".into()), None, 0, true),
            "● DIM"
        );
    }

    #[test]
    fn a_working_run_turns_the_spinner_once_a_frame() {
        let frames: Vec<String> = (0..5)
            .map(|i| text(&Status::Working, None, i * SPINNER_FRAME_MS, true))
            .collect();
        assert_eq!(frames, ["◐ DIM", "◓ DIM", "◑ DIM", "◒ DIM", "◐ DIM"]);
    }

    #[test]
    fn a_yielded_title_is_the_plain_text_and_turns_no_frames() {
        for status in [
            Status::Working,
            Status::Permission("exec".into()),
            Status::Question,
            Status::Idle,
            Status::Done,
            Status::Error,
        ] {
            for ms in [0, 130, 999] {
                assert_eq!(
                    text(&status, Some("hi"), ms, false),
                    "DIM | hi",
                    "{status:?} {ms}"
                );
            }
        }
        let start = Instant::now();
        let mut title = WindowTitle::new(false, false, start);
        assert_eq!(
            title.report(&Status::Working, Some("hi"), start),
            Some(b"\x1b]0;DIM | hi\x07".to_vec())
        );
        for ms in [130, 260, 900] {
            assert_eq!(
                title.report(
                    &Status::Working,
                    Some("hi"),
                    start + Duration::from_millis(ms)
                ),
                None,
                "{ms}"
            );
        }
        assert_eq!(
            title.report(
                &Status::Permission("exec".into()),
                Some("hi"),
                start + Duration::from_millis(1000)
            ),
            None
        );
    }

    #[test]
    fn no_record_means_the_mark_stays() {
        assert!(marks(false));
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
        let mut title = WindowTitle::new(false, true, start);
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
        let mut apple = WindowTitle::new(true, true, start);
        assert_eq!(
            apple.report(&Status::Idle, Some("hi"), start),
            Some(b"\x1b]2;DIM | hi\x07".to_vec())
        );
    }
}
