//! Desktop notifications for a run that needs the user back (port of the TS
//! `terminal-notification.ts`): it finished, or it waits on a permission or a
//! question. The interface owns nothing outside the terminal, so it asks the
//! terminal to notify, in the one form each understands: iTerm2 `OSC 9`,
//! kitty `OSC 99`, the rxvt `OSC 777` notify of Ghostty, WezTerm and foot.
//! Anything else, and every multiplexer (tmux forwards a bell by default but
//! needs passthrough for an OSC), gets a plain `BEL`, which every terminal
//! turns into some signal.
//!
//! On by default; `/settings` switches it (`tuiNotifyOnComplete`).

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
}

/// The terminal as notifications see it.
#[derive(Debug, Default)]
pub struct Terminal {
    pub term_program: String,
    pub term: String,
    pub kitty: bool,
    pub multiplexer: bool,
}

impl Terminal {
    pub fn from_env() -> Self {
        let var = |name| std::env::var(name).unwrap_or_default();
        Self {
            term_program: var("TERM_PROGRAM"),
            term: var("TERM"),
            kitty: !var("KITTY_WINDOW_ID").is_empty(),
            multiplexer: !var("TMUX").is_empty() || var("HERDR_ENV") == "1",
        }
    }
}

/// The bytes that make `terminal` show `event`; never empty.
pub fn sequence(terminal: &Terminal, event: &Event) -> Vec<u8> {
    let body = match event {
        Event::Complete => t("Task complete").to_owned(),
        Event::Permission(tool) => tf("Permission needed: {tool}", &[("tool", tool)]),
        Event::Question => t("Question waiting for your answer").to_owned(),
    };
    // `;` ends an OSC parameter and control characters end the sequence.
    let clean = |text: &str| -> String {
        text.chars()
            .filter(|c| !c.is_control())
            .map(|c| if c == ';' { ',' } else { c })
            .collect()
    };
    let (title, body) = ("DIM", clean(&body));
    let program = terminal.term_program.as_str();
    let term = terminal.term.as_str();
    let text = if terminal.multiplexer {
        "\x07".to_owned()
    } else if program == "iTerm.app" {
        format!("\x1b]9;{title}: {body}\x07")
    } else if term == "xterm-kitty" || terminal.kitty {
        format!("\x1b]99;;{title}: {body}\x1b\\")
    } else if matches!(program, "ghostty" | "WezTerm")
        || term == "xterm-ghostty"
        || term.starts_with("foot")
        || term.starts_with("rxvt-unicode")
    {
        format!("\x1b]777;notify;{title};{body}\x07")
    } else {
        "\x07".to_owned()
    };
    text.into_bytes()
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
        assert_eq!(sequence(&on("Apple_Terminal", "xterm"), &done), b"\x07");
        let tmux = Terminal {
            multiplexer: true,
            ..on("iTerm.app", "tmux-256color")
        };
        assert_eq!(sequence(&tmux, &Event::Question), b"\x07");
    }
}
