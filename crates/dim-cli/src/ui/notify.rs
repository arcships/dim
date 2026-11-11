//! Desktop notifications for a run that needs the user back (port of the TS
//! `terminal-notification.ts`): it finished, or it waits on a permission or a
//! question. The interface owns nothing outside the terminal, so it asks the
//! terminal to notify, in the one form each understands: iTerm2 `OSC 9`,
//! kitty `OSC 99`, the rxvt `OSC 777` notify of Ghostty, WezTerm and foot.
//! Anything else gets a plain `BEL`, which every terminal turns into some
//! signal.
//!
//! What a multiplexer gets differs, because neither carries the form upward
//! by itself and each drops a different part of it. tmux 3.3 and later leave
//! `allow-passthrough` off, so a wrapped sequence may be discarded whole: the
//! form is wrapped as tmux's passthrough asks (`OSC 52` and the rest of what
//! dim writes there) and a bare `BEL` follows it, so a tmux that forwards the
//! form draws a rich notification and one that does not still rings. GNU
//! screen does not double `ESC` (`ESC P` is its own passthrough, which a
//! notification does not need), and some builds forward an `OSC` closed by
//! `BEL` while others drop every `OSC` a pane writes: the bell-terminated
//! forms go through unwrapped with a bare `BEL` after them, which rings on
//! both. kitty's `OSC 99`, closed by `ST`, has no shape screen carries whole,
//! so it becomes the bell alone. A notification asserts nothing after it is
//! delivered, so the record's reason for staying inside a multiplexer
//! (`ui::status`, `ui::progress`) does not apply.
//!
//! herdr is not a multiplexer of that kind and gets the bell. It reads the
//! `OSC`s in a pane for itself and notifies from its own list of terminals,
//! so a form the pane's variables name is not known to reach the user there,
//! while a `BEL` is not an `OSC` it would take. `HERDR_ENV` therefore sends
//! the bell whatever `TERM_PROGRAM` says.
//!
//! On by default; `/settings` switches it (`tuiNotifyOnComplete`).

use crate::model::view::TaskOutcome;
use crate::ui::i18n::{t, tf};

/// `config.json` key of the switch; absent means on.
pub const NOTIFY_KEY: &str = "tuiNotifyOnComplete";

/// Why the run wants the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Complete,
    /// A permission request for the named tool.
    Permission(String),
    Question,
    /// A background task that ran apart from the turn ended: its label, and
    /// how it ended.
    BackgroundFinished {
        label: String,
        outcome: TaskOutcome,
    },
}

/// The multiplexer a notification is written inside, if any.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Multiplexer {
    #[default]
    None,
    Tmux,
    Screen,
}

impl Multiplexer {
    /// The multiplexer these variables describe: tmux by `TMUX` or a `tmux`
    /// `TERM`, GNU screen by `STY` or a `screen` `TERM`.
    fn of(var: impl Fn(&str) -> Option<String>) -> Self {
        let var = |name: &str| var(name).unwrap_or_default();
        if !var("TMUX").is_empty() {
            return Self::Tmux;
        }
        if !var("STY").is_empty() {
            return Self::Screen;
        }
        let term = var("TERM");
        if term.starts_with("tmux") {
            return Self::Tmux;
        }
        if term.starts_with("screen") {
            return Self::Screen;
        }
        Self::None
    }
}

/// The terminal as notifications see it.
#[derive(Debug, Default)]
pub struct Terminal {
    pub term_program: String,
    pub term: String,
    pub kitty: bool,
    pub multiplexer: Multiplexer,
    /// herdr reads a pane's `OSC`s for itself, so nothing shows a form the
    /// pane's variables name; a `BEL` is what it does not take.
    pub herdr: bool,
}

impl Terminal {
    pub fn from_env() -> Self {
        Self::of(|name| std::env::var(name).ok())
    }

    /// The terminal these variables describe.
    pub fn of(var: impl Fn(&str) -> Option<String>) -> Self {
        let value = |name: &str| var(name).unwrap_or_default();
        Self {
            term_program: value("TERM_PROGRAM"),
            term: value("TERM"),
            kitty: !value("KITTY_WINDOW_ID").is_empty(),
            multiplexer: Multiplexer::of(&var),
            herdr: value("HERDR_ENV") == "1",
        }
    }

    /// Whether an `OSC` written here stops before the terminal the user looks
    /// at: a multiplexer keeps a sequence it does not know, and herdr reads a
    /// pane's `OSC`s for its own terminals. The record and the bar are drawn
    /// only where nothing stands between them and the user (`ui::progress`);
    /// a notification, which asserts nothing, is wrapped or reduced to the
    /// bell instead (`sequence`).
    pub fn intercepts_osc(&self) -> bool {
        self.multiplexer != Multiplexer::None || self.herdr
    }
}

/// The bytes that make `terminal` show `event`; never empty. Inside tmux the
/// wrapped form is followed by a bare `BEL`, and inside GNU screen a
/// bell-terminated form is passed as it is with a bare `BEL` after it, so the
/// output under a multiplexer may hold both a sequence and a bell while every
/// other case is one form or the bell alone.
pub fn sequence(terminal: &Terminal, event: &Event) -> Vec<u8> {
    if terminal.herdr {
        return b"\x07".to_vec();
    }
    let body = match event {
        Event::Complete => t("Task complete").to_owned(),
        Event::Permission(tool) => tf("Permission needed: {tool}", &[("tool", tool)]),
        Event::Question => t("Question waiting for your answer").to_owned(),
        Event::BackgroundFinished { label, outcome } => tf(
            match outcome {
                TaskOutcome::Completed => "Background task finished: {label}",
                TaskOutcome::Failed => "Background task failed: {label}",
                TaskOutcome::Stopped => "Background task stopped: {label}",
            },
            &[("label", label)],
        ),
    };
    let clean = |text: &str| -> String {
        text.chars()
            .filter(|c| !c.is_control())
            .map(|c| if c == ';' { ',' } else { c })
            .collect()
    };
    let (title, body) = ("DIM", clean(&body));
    let program = terminal.term_program.as_str();
    let term = terminal.term.as_str();
    let form = if program == "iTerm.app" {
        Some(format!("\x1b]9;{title}: {body}\x07"))
    } else if term == "xterm-kitty" || terminal.kitty {
        Some(format!("\x1b]99;;{title}: {body}\x1b\\"))
    } else if matches!(program, "ghostty" | "WezTerm")
        || term == "xterm-ghostty"
        || term.starts_with("foot")
        || term.starts_with("rxvt-unicode")
    {
        Some(format!("\x1b]777;notify;{title};{body}\x07"))
    } else {
        None
    };
    match form {
        Some(text) => multiplexed(terminal.multiplexer, text.into_bytes()),
        None => b"\x07".to_vec(),
    }
}

/// `sequence` as `multiplexer` needs it.
///
/// tmux may drop a passthrough whole (3.3 and later default
/// `allow-passthrough` to off), so the wrapped form is followed by a bare
/// `BEL`: a tmux that forwards it draws the notification, one that does not
/// still rings. GNU screen is the same rule for the same reason: some builds
/// forward a `BEL`-terminated `OSC` a pane writes and some drop every `OSC`,
/// so a bell-terminated form is sent as it is and a bare `BEL` follows it.
/// `OSC 99`, closed by `ST`, is not: `ESC P` would start a passthrough screen
/// does not terminate the way tmux does, so only the bell is sent.
fn multiplexed(multiplexer: Multiplexer, sequence: Vec<u8>) -> Vec<u8> {
    match multiplexer {
        Multiplexer::None => sequence,
        Multiplexer::Tmux => {
            let mut wrapped = passthrough(&sequence);
            wrapped.push(0x07);
            wrapped
        }
        Multiplexer::Screen if sequence.last() == Some(&0x07) => {
            let mut passed = sequence;
            passed.push(0x07);
            passed
        }
        Multiplexer::Screen => b"\x07".to_vec(),
    }
}

/// `sequence` in tmux's passthrough form: past `ESC P tmux;`, every `ESC` of
/// the sequence is doubled so tmux reads the inner sequence as one string
/// instead of acting on it. `BEL` needs no escaping, so a notification body
/// never ends the string early.
fn passthrough(sequence: &[u8]) -> Vec<u8> {
    let mut escaped = Vec::with_capacity(sequence.len() * 2);
    for &byte in sequence {
        if byte == 0x1b {
            escaped.push(0x1b);
        }
        escaped.push(byte);
    }
    let mut wrapped = Vec::with_capacity(b"\x1bPtmux;".len() + escaped.len() + 2);
    wrapped.extend_from_slice(b"\x1bPtmux;");
    wrapped.extend_from_slice(&escaped);
    wrapped.extend_from_slice(b"\x1b\\");
    wrapped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(term_program: &str, term: &str) -> Terminal {
        Terminal {
            term_program: term_program.into(),
            term: term.into(),
            ..Terminal::default()
        }
    }

    fn in_mux(multiplexer: Multiplexer, term_program: &str, term: &str) -> Terminal {
        Terminal {
            multiplexer,
            ..on(term_program, term)
        }
    }

    #[test]
    fn each_terminal_gets_the_form_it_understands() {
        let done = Event::Complete;
        assert_eq!(
            sequence(&on("iTerm.app", "xterm-256color"), &done),
            b"\x1b]9;DIM: Task complete\x07"
        );
        assert_eq!(
            sequence(&on("", "xterm-kitty"), &done),
            b"\x1b]99;;DIM: Task complete\x1b\\"
        );
        assert_eq!(
            sequence(
                &on("ghostty", "xterm-ghostty"),
                &Event::Permission("bash;rm".into())
            ),
            "\x1b]777;notify;DIM;Permission needed: bash,rm\x07".as_bytes()
        );
        assert_eq!(
            sequence(
                &on("ghostty", "xterm-ghostty"),
                &Event::Permission("ba\u{7}sh\u{1b}".into())
            ),
            "\x1b]777;notify;DIM;Permission needed: bash\x07".as_bytes()
        );
        assert_eq!(sequence(&on("Apple_Terminal", "xterm"), &done), b"\x07");
        assert_eq!(
            sequence(
                &on("iTerm.app", "xterm-256color"),
                &Event::BackgroundFinished {
                    label: "cargo build; rm -rf /".into(),
                    outcome: TaskOutcome::Completed,
                }
            ),
            b"\x1b]9;DIM: Background task finished: cargo build, rm -rf /\x07"
        );
        assert_eq!(
            sequence(
                &on("ghostty", "xterm-ghostty"),
                &Event::BackgroundFinished {
                    label: "cargo test".into(),
                    outcome: TaskOutcome::Failed,
                }
            ),
            "\x1b]777;notify;DIM;Background task failed: cargo test\x07".as_bytes()
        );
        assert_eq!(
            sequence(
                &on("iTerm.app", "xterm-256color"),
                &Event::BackgroundFinished {
                    label: "cargo test".into(),
                    outcome: TaskOutcome::Stopped,
                }
            ),
            b"\x1b]9;DIM: Background task stopped: cargo test\x07"
        );
        assert_eq!(
            sequence(
                &in_mux(Multiplexer::Tmux, "iTerm.app", "tmux-256color"),
                &Event::BackgroundFinished {
                    label: "cargo build".into(),
                    outcome: TaskOutcome::Completed,
                }
            ),
            b"\x1bPtmux;\x1b\x1b]9;DIM: Background task finished: cargo build\x07\x1b\\\x07"
        );
        assert_eq!(
            sequence(
                &in_mux(Multiplexer::Screen, "", "xterm-kitty"),
                &Event::BackgroundFinished {
                    label: "cargo build".into(),
                    outcome: TaskOutcome::Failed,
                }
            ),
            b"\x07"
        );
    }

    #[test]
    fn a_multiplexer_gets_what_it_forwards_and_otherwise_a_bell() {
        let question = Event::Question;
        assert_eq!(
            sequence(
                &in_mux(Multiplexer::Tmux, "iTerm.app", "tmux-256color"),
                &question
            ),
            b"\x1bPtmux;\x1b\x1b]9;DIM: Question waiting for your answer\x07\x1b\\\x07"
        );
        assert_eq!(
            sequence(
                &in_mux(Multiplexer::Tmux, "ghostty", "tmux-256color"),
                &question
            ),
            "\x1bPtmux;\x1b\x1b]777;notify;DIM;Question waiting for your answer\x07\x1b\\\x07"
                .as_bytes()
        );
        assert_eq!(
            sequence(
                &in_mux(Multiplexer::Screen, "iTerm.app", "screen-256color"),
                &question
            ),
            b"\x1b]9;DIM: Question waiting for your answer\x07\x07"
        );
        assert_eq!(
            sequence(
                &in_mux(Multiplexer::Screen, "ghostty", "screen-256color"),
                &question
            ),
            "\x1b]777;notify;DIM;Question waiting for your answer\x07\x07".as_bytes()
        );
        assert_eq!(
            sequence(&in_mux(Multiplexer::Tmux, "", "xterm-kitty"), &question),
            "\x1bPtmux;\x1b\x1b]99;;DIM: Question waiting for your answer\x1b\x1b\\\x1b\\\x07"
                .as_bytes()
        );
        assert_eq!(
            sequence(&in_mux(Multiplexer::Screen, "", "xterm-kitty"), &question),
            b"\x07"
        );
        assert_eq!(
            sequence(
                &in_mux(Multiplexer::Tmux, "Apple_Terminal", "tmux-256color"),
                &question
            ),
            b"\x07"
        );
    }

    #[test]
    fn a_passthrough_that_tmux_drops_still_rings() {
        let question = Event::Question;
        for (term_program, term) in [
            ("iTerm.app", "tmux-256color"),
            ("ghostty", "tmux-256color"),
            ("", "xterm-kitty"),
        ] {
            let bytes = sequence(&in_mux(Multiplexer::Tmux, term_program, term), &question);
            assert!(
                bytes.starts_with(b"\x1bPtmux;"),
                "{term_program:?} {term:?}"
            );
            assert!(bytes.ends_with(b"\x07"), "{term_program:?} {term:?}");
        }
    }

    #[test]
    fn screen_gets_no_doubled_escape_and_no_unterminated_osc() {
        let question = Event::Question;
        for (term_program, term) in [
            ("iTerm.app", "screen-256color"),
            ("ghostty", "screen-256color"),
            ("WezTerm", "screen-256color"),
        ] {
            let bytes = sequence(&in_mux(Multiplexer::Screen, term_program, term), &question);
            assert!(
                !bytes.windows(2).any(|pair| pair == b"\x1b\x1b"),
                "{term_program:?} {term:?}"
            );
            assert!(bytes.ends_with(b"\x07"), "{term_program:?} {term:?}");
        }
    }

    #[test]
    fn herdr_gets_the_bell_whatever_terminal_it_sits_on() {
        let question = Event::Question;
        assert_eq!(
            sequence(
                &Terminal {
                    herdr: true,
                    ..on("ghostty", "xterm-ghostty")
                },
                &question
            ),
            b"\x07"
        );
        assert_eq!(
            sequence(
                &Terminal {
                    herdr: true,
                    ..on("iTerm.app", "xterm-256color")
                },
                &question
            ),
            b"\x07"
        );
        for (term_program, term) in [("", "xterm-kitty"), ("", "xterm-256color")] {
            assert_eq!(
                sequence(
                    &Terminal {
                        herdr: true,
                        ..on(term_program, term)
                    },
                    &Event::Complete
                ),
                b"\x07",
                "{term:?}"
            );
        }
        assert_eq!(
            sequence(
                &Terminal {
                    herdr: true,
                    ..in_mux(Multiplexer::Tmux, "iTerm.app", "tmux-256color")
                },
                &Event::Complete
            ),
            b"\x07"
        );
    }

    #[test]
    fn the_multiplexer_is_named_by_its_own_variable() {
        let of = |pairs: &[(&str, &str)]| {
            Multiplexer::of(|name| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            })
        };
        assert_eq!(
            of(&[("TMUX", "/tmp/tmux-501/default,1,0")]),
            Multiplexer::Tmux
        );
        assert_eq!(of(&[("STY", "1.pts-0.host")]), Multiplexer::Screen);
        assert_eq!(of(&[("TERM", "screen-256color")]), Multiplexer::Screen);
        assert_eq!(
            of(&[("TMUX", "x"), ("STY", "y"), ("TERM", "screen")]),
            Multiplexer::Tmux
        );
        assert_eq!(
            of(&[("HERDR_ENV", "1"), ("TERM", "xterm-256color")]),
            Multiplexer::None
        );
        assert_eq!(of(&[("TERM", "tmux-256color")]), Multiplexer::Tmux);
        assert_eq!(of(&[]), Multiplexer::None);
    }
}
