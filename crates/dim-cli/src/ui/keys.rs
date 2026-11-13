//! `/keys`: the keys of the interface, and in scrollback how the terminal
//! that owns the history scrolls, searches and copies (port of the TS
//! `terminal-hints.ts` and `buildKeysBlockLines`).

use crate::model::session::SubmitMode;
use crate::ui::i18n::{t, tf};

/// How a terminal reaches its own history: scroll, search, copy.
struct History {
    scroll: String,
    search: String,
    copy: String,
}

fn history(platform: &str, var: &dyn Fn(&str) -> String) -> History {
    let of = |scroll: &str, search: &str, copy: String| History {
        scroll: scroll.to_owned(),
        search: search.to_owned(),
        copy,
    };
    let drag_or = |shortcut: &str| tf("drag or {shortcut}", &[("shortcut", shortcut)]);
    let program = var("TERM_PROGRAM").to_lowercase();
    if !var("TMUX").is_empty() {
        return of(
            "prefix+[",
            "copy-mode /",
            "copy-mode Space then Enter".to_owned(),
        );
    }
    if !var("WT_SESSION").is_empty() || platform == "windows" {
        return of("Ctrl+Shift+PageUp", "Ctrl+Shift+F", drag_or("Ctrl+Shift+C"));
    }
    if program == "apple_terminal" || program == "iterm.app" {
        return of("Cmd+PageUp", "Cmd+F", drag_or("Cmd+C"));
    }
    let term = var("TERM");
    if ["ghostty", "wezterm", "kitty", "alacritty"]
        .iter()
        .any(|name| program.contains(name) || term.contains(name))
    {
        return of(
            "Shift+PageUp",
            t("terminal search"),
            drag_or("Ctrl+Shift+C"),
        );
    }
    match platform {
        "macos" => of("Cmd+PageUp", "Cmd+F", t("drag to select").to_owned()),
        "linux" => of("Shift+PageUp", "Ctrl+Shift+F", "Ctrl+Shift+C".to_owned()),
        _ => of(
            t("terminal scrollback"),
            t("terminal search"),
            t("terminal copy").to_owned(),
        ),
    }
}

/// The `/keys` report, one string per row.
pub fn report(fullscreen: bool, mouse: bool, default_send_mode: SubmitMode) -> Vec<String> {
    let steer_keys = match default_send_mode {
        SubmitMode::Queue => "Enter / Ctrl+S",
        SubmitMode::Steer => "Enter",
    };
    let mut rows = vec![
        t("Application keys").to_owned(),
        format!(
            "  {}",
            t("Enter submit when idle · Shift+Enter / Alt+Enter / Ctrl+J newline · ↑↓ history")
        ),
        format!(
            "  {}",
            tf(
                "Running with a draft: Enter {default} · Ctrl+S {alternate}",
                &[
                    ("default", default_send_mode.as_str()),
                    ("alternate", default_send_mode.opposite().as_str()),
                ],
            )
        ),
        format!(
            "  {}",
            tf(
                "Empty draft: {keys} steer first queued",
                &[("keys", steer_keys)]
            )
        ),
        format!(
            "  {}",
            t(
                "Ctrl+O or Ctrl+P commands · Esc interrupt, queued messages return · Shift+← edit last queued"
            )
        ),
        format!(
            "  {}",
            t("Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft")
        ),
        format!("  {}", t("Shift+Tab cycle permissions (composer focused)")),
    ];
    if fullscreen {
        rows.push(t("Fullscreen").to_owned());
        rows.push(format!(
            "  {}",
            t("PageUp/PageDown scroll · Home/End top or bottom (empty draft) · Tab pick a message, e edit it, q quote it")
        ));
        rows.push(format!(
            "  {}",
            t("Tab also picks tool headers · Enter/Space toggle the focused tool")
        ));
        rows.push(format!(
            "  {}",
            t("Shift+Tab newer target while picked · Esc return to composer")
        ));
        rows.push(format!(
            "  {}",
            if mouse {
                t("wheel scroll · drag select and copy · click a link to open it, a tool group to expand it")
            } else {
                t("mouse off (/settings): the terminal selects and copies")
            }
        ));
    } else {
        let var = |name: &str| std::env::var(name).unwrap_or_default();
        let history = history(std::env::consts::OS, &var);
        rows.push(t("Terminal-owned history").to_owned());
        rows.push(format!(
            "  {}",
            tf("scroll: {value}", &[("value", &history.scroll)])
        ));
        rows.push(format!(
            "  {}",
            tf("search: {value}", &[("value", &history.search)])
        ));
        rows.push(format!(
            "  {}",
            tf("copy: {value}", &[("value", &history.copy)])
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_report_keeps_shortcut_only_actions_and_distinguishes_modes() {
        for fullscreen in [false, true] {
            let report = report(fullscreen, false, SubmitMode::Queue).join("\n");
            assert!(
                report.contains("Esc interrupt, queued messages return"),
                "{report}"
            );
            assert!(report.contains("Enter queue · Ctrl+S steer"), "{report}");
            assert!(report.contains("Shift+Tab cycle permissions"), "{report}");
            if fullscreen {
                assert!(
                    report.contains("Home/End top or bottom (empty draft)"),
                    "{report}"
                );
                assert!(report.contains("Tab pick a message"), "{report}");
                assert!(!report.contains("Terminal-owned history"), "{report}");
            } else {
                assert!(report.contains("Terminal-owned history"), "{report}");
                assert!(!report.contains("Home/End"), "{report}");
                assert!(!report.contains("Tab pick a message"), "{report}");
            }
        }
        let previous = crate::ui::i18n::active();
        for (locale, code, _) in crate::ui::i18n::LOCALES {
            if locale == crate::ui::i18n::Locale::En {
                continue;
            }
            crate::ui::i18n::set(locale);
            for fullscreen in [false, true] {
                let report = report(fullscreen, false, SubmitMode::Queue).join("\n");
                for (key, visible) in [
                    ("Shift+Tab cycle permissions (composer focused)", true),
                    (
                        "Shift+Tab newer target while picked · Esc return to composer",
                        fullscreen,
                    ),
                ] {
                    let translated = t(key);
                    assert_ne!(translated, key, "{code} lacks {key:?}");
                    assert_eq!(report.contains(translated), visible, "{code}: {report}");
                }
            }
            let hint = "(shift+tab to cycle)";
            assert_ne!(t(hint), hint, "{code} lacks {hint:?}");
        }
        crate::ui::i18n::set(previous);
    }

    #[test]
    fn the_terminal_decides_how_its_history_is_reached() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map_or(String::new(), |(_, v)| (*v).to_owned())
            }
        };
        let tmux = history(
            "macos",
            &env(&[("TMUX", "/tmp/x"), ("TERM_PROGRAM", "iTerm.app")]),
        );
        assert_eq!(tmux.scroll, "prefix+[");
        let iterm = history("macos", &env(&[("TERM_PROGRAM", "iTerm.app")]));
        assert_eq!(
            (iterm.scroll.as_str(), iterm.copy.as_str()),
            ("Cmd+PageUp", "drag or Cmd+C")
        );
        let kitty = history("linux", &env(&[("TERM_PROGRAM", "kitty")]));
        assert_eq!(kitty.search, "terminal search");
        let ghostty_over_ssh = history("linux", &env(&[("TERM", "xterm-ghostty")]));
        assert_eq!(ghostty_over_ssh.search, "terminal search");
        let linux = history("linux", &env(&[]));
        assert_eq!(linux.copy, "Ctrl+Shift+C");
    }
}
