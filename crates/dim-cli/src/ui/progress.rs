//! The run's progress as the terminal draws it: OSC 9;4, the progress
//! sequence of ConEmu and Windows Terminal, which Ghostty, iTerm2 and WezTerm
//! draw too (in the tab or title bar; Windows also on the taskbar button).
//!
//! The bar is a second view of the program status (`ui::status`), for the
//! terminals that draw a bar but keep no record: indeterminate while a run
//! works, paused while it waits on a permission or a question, none
//! otherwise. A bar says a run is under way or held up, which a finished or
//! failed run is not, so `done` and `error` remove it as `idle` does; the
//! result stays in the status record and the transcript.
//!
//! OSC 9;4 shares its number with iTerm2's notification, and a terminal that
//! knows only the notification shows every update as one (iTerm2 before
//! 3.6.6). The bar is therefore drawn only where it is known to work. Not
//! inside tmux, GNU screen or herdr: tmux and screen keep the sequence, the
//! outer terminal's variables leak into the pane, and passed through, the
//! outer terminal would show one bar for all panes and keep a killed pane's.
//! A terminal that answered the startup probe's program status query keeps
//! the record (`ui::status`) and draws its own bar from it, so none is drawn
//! there either. `DIM_TERMINAL_PROGRESS=off` turns the bar off and `on` draws
//! it anywhere; `DIM_TERMINAL_STATUS` is about the status record only.
//!
//! Ghostty drops a bar that is not repeated within about 15 seconds, so a
//! bar that shows is drawn again every second. Once one is drawn, handing
//! the terminal back removes it (`terminal::modes::Mode::Progress`).

use std::time::{Duration, Instant};

use crate::ui::notify::Terminal;
use crate::ui::status::Status;
use crate::ui::terminal::modes::Mode;
use crate::ui::theme::detect::Env;

/// The environment switch: `off` draws no bar, `on` draws it in any terminal.
const ENV_KEY: &str = "DIM_TERMINAL_PROGRESS";
/// How often a bar that shows is drawn again; Ghostty asks for at least once
/// a second.
const KEEPALIVE: Duration = Duration::from_secs(1);
/// Indeterminate: the terminal animates its own bar.
const BUSY: &[u8] = b"\x1b]9;4;3\x07";
/// Paused, in the terminal's warning colour. The value is sent because an
/// omitted one keeps whatever width the bar had, and an empty paused bar
/// cannot be told from no bar.
const PAUSED: &[u8] = b"\x1b]9;4;4;100\x07";

/// The bar for `status`; `None` for no bar.
fn bar(status: &Status) -> Option<&'static [u8]> {
    match status {
        Status::Working => Some(BUSY),
        Status::Permission(_) | Status::Question => Some(PAUSED),
        Status::Idle | Status::Done | Status::Error => None,
    }
}

/// Whether the terminal `env` describes draws the bar, `program_status` being
/// whether it keeps the record of the run. A multiplexer or herdr between the
/// pane and the user hides the bar, which is why `notify::Terminal` decides
/// that part here too.
fn draws(env: &Env, program_status: bool) -> bool {
    let var = |key: &str| env.get(key).map_or("", String::as_str);
    match var(ENV_KEY).trim().to_ascii_lowercase().as_str() {
        "off" => return false,
        "on" => return true,
        _ => {}
    }
    if program_status || Terminal::of(|key| env.get(key).cloned()).intercepts_osc() {
        return false;
    }
    let program = var("TERM_PROGRAM");
    !var("WT_SESSION").is_empty()
        || var("ConEmuANSI") == "ON"
        || program == "ghostty"
        || var("TERM") == "xterm-ghostty"
        || program == "WezTerm"
        || (program == "iTerm.app" && release(var("TERM_PROGRAM_VERSION")) >= [3, 6, 6])
}

/// Major, minor and patch of a dotted version, by the leading number of
/// each part: `3.7.0beta6` is `[3, 7, 0]`, and `3.6` is `[3, 6, 0]`.
fn release(version: &str) -> [u32; 3] {
    let mut parts = version.split('.').map(|part| {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().unwrap_or(0)
    });
    std::array::from_fn(|_| parts.next().unwrap_or(0))
}

/// Draws the bar for each status, and again every second while one shows.
#[derive(Debug)]
pub struct ProgressBar {
    enabled: bool,
    /// The bar the terminal was last given, and when; `None` while it shows
    /// none.
    shown: Option<(&'static [u8], Instant)>,
}

impl ProgressBar {
    /// Draws where the terminal is known to and keeps no record of the run
    /// itself, or as `DIM_TERMINAL_PROGRESS` says.
    pub fn from_env(env: &Env, program_status: bool) -> Self {
        Self::new(draws(env, program_status))
    }

    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            shown: None,
        }
    }

    /// What brings the terminal's bar to `status` at `now`: a new bar, the
    /// one that shows once a second has passed, or the removal of the last.
    pub fn report(&mut self, status: &Status, now: Instant) -> Option<&'static [u8]> {
        if !self.enabled {
            return None;
        }
        match (bar(status), self.shown) {
            (None, None) => None,
            (None, Some(_)) => {
                self.shown = None;
                Some(Mode::Progress.restore_sequence())
            }
            (Some(bar), Some((shown, at))) if bar == shown && now - at < KEEPALIVE => None,
            (Some(bar), _) => {
                self.shown = Some((bar, now));
                Some(bar)
            }
        }
    }

    /// The terminal was handed back, which removed the bar: the next status
    /// draws it again.
    pub fn forget(&mut self) {
        self.shown = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_working_or_held_up_shows_a_bar_and_nothing_else_does() {
        assert_eq!(bar(&Status::Working), Some(&b"\x1b]9;4;3\x07"[..]));
        assert_eq!(
            bar(&Status::Permission("exec".into())),
            Some(&b"\x1b]9;4;4;100\x07"[..])
        );
        assert_eq!(bar(&Status::Question), Some(&b"\x1b]9;4;4;100\x07"[..]));
        for status in [Status::Idle, Status::Done, Status::Error] {
            assert_eq!(bar(&status), None, "{status:?}");
        }
        assert_eq!(Mode::Progress.restore_sequence(), b"\x1b]9;4;0\x07");
    }

    #[test]
    fn only_terminals_known_to_draw_the_bar_get_it() {
        let env = |pairs: &[(&str, &str)]| -> Env {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let ghostty = [("TERM_PROGRAM", "ghostty"), ("TERM", "xterm-ghostty")];
        for pairs in [
            &[("WT_SESSION", "0f8a")][..],
            &[("ConEmuANSI", "ON")],
            &ghostty,
            &[("TERM", "xterm-ghostty")],
            &[("TERM_PROGRAM", "WezTerm")],
            &[
                ("TERM_PROGRAM", "iTerm.app"),
                ("TERM_PROGRAM_VERSION", "3.6.6"),
            ],
            &[
                ("TERM_PROGRAM", "iTerm.app"),
                ("TERM_PROGRAM_VERSION", "3.7.0beta6"),
            ],
            &[
                ("TERM_PROGRAM", "iTerm.app"),
                ("TERM_PROGRAM_VERSION", "4.0"),
            ],
            &[("TERM_PROGRAM", "Apple_Terminal"), (ENV_KEY, " On ")],
            &[("TMUX", "/tmp/tmux-501/default,1,0"), (ENV_KEY, "on")],
            &[
                ("TERM_PROGRAM", "rex"),
                ("TERM", "xterm-rex"),
                (ENV_KEY, "on"),
            ],
        ] {
            assert!(draws(&env(pairs), false), "{pairs:?}");
        }
        for pairs in [
            &[][..],
            &[("TERM_PROGRAM", "Apple_Terminal")],
            &[("TERM", "xterm-kitty")],
            &[("TERM_PROGRAM", "rex"), ("TERM", "xterm-rex")],
            &[
                ("TERM_PROGRAM", "iTerm.app"),
                ("TERM_PROGRAM_VERSION", "3.6.5"),
            ],
            &[
                ("TERM_PROGRAM", "iTerm.app"),
                ("TERM_PROGRAM_VERSION", "3.6"),
            ],
            &[("TERM_PROGRAM", "iTerm.app")],
            &[
                ("TMUX", "/tmp/tmux-501/default,1,0"),
                ("WT_SESSION", "0f8a"),
            ],
            &[("STY", "1.pts-0.host"), ("TERM", "xterm-ghostty")],
            &[("TERM", "screen-256color")],
            &[("HERDR_ENV", "1"), ("TERM_PROGRAM", "ghostty")],
            &[ghostty[0], ghostty[1], (ENV_KEY, "OFF")],
        ] {
            assert!(!draws(&env(pairs), false), "{pairs:?}");
        }
        for pairs in [
            &[][..],
            &[("WT_SESSION", "0f8a")],
            &ghostty,
            &[
                ("TERM_PROGRAM", "iTerm.app"),
                ("TERM_PROGRAM_VERSION", "4.0"),
            ],
        ] {
            assert!(!draws(&env(pairs), true), "{pairs:?}");
        }
        assert!(draws(
            &env(&[("WT_SESSION", "0f8a"), (ENV_KEY, "on")]),
            true
        ));
        assert!(!draws(
            &env(&[("WT_SESSION", "0f8a"), (ENV_KEY, "off")]),
            true
        ));
    }

    #[test]
    fn the_bar_is_drawn_on_a_change_and_again_every_second() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut progress = ProgressBar::new(true);
        let permission = Status::Permission("exec".into());
        let steps = [
            (Status::Idle, 0, None),
            (Status::Working, 0, Some(BUSY)),
            (Status::Working, 999, None),
            (Status::Working, 1000, Some(BUSY)),
            (permission.clone(), 1100, Some(PAUSED)),
            (Status::Question, 1500, None),
            (Status::Question, 2100, Some(PAUSED)),
            (Status::Error, 2200, Some(Mode::Progress.restore_sequence())),
            (Status::Done, 9000, None),
            (Status::Working, 9000, Some(BUSY)),
        ];
        for (step, (status, ms, drawn)) in steps.into_iter().enumerate() {
            assert_eq!(progress.report(&status, at(ms)), drawn, "step {step}");
        }
        progress.forget();
        assert_eq!(progress.report(&Status::Working, at(9001)), Some(BUSY));
        let mut off = ProgressBar::new(false);
        assert_eq!(off.report(&Status::Working, at(0)), None);
        assert_eq!(off.report(&permission, at(5000)), None);
    }
}
