//! The interface language (port of the TS `i18n` locale model). Strings are
//! keyed by their English text: [`t`] looks a string up in the active
//! locale's table and falls back to the English. Every locale's table holds
//! the keys of the Simplified Chinese one, which follows the TS build's
//! `zh-CN` catalogue.
//!
//! The active locale belongs to the interface thread: every panel, hint and
//! notice reads it while drawing, so a switch shows on the next frame. Rows
//! already committed to the transcript keep their language.

mod de;
mod es;
mod fr;
mod id;
mod ja;
mod ko;
mod pt_br;
mod pt_pt;
mod zh_cn;
mod zh_hant;

use std::cell::Cell;

pub use crate::locale::{LOCALES, Locale};

/// `config.json` key of the saved locale; absent means the system default.
pub const LOCALE_KEY: &str = "locale";

/// The locale the environment asks for (`LC_ALL`, `LC_MESSAGES`, `LANG`),
/// English when none names a known language.
pub fn system_locale() -> Locale {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
        .and_then(|value| Locale::from_env_value(&value))
        .unwrap_or(Locale::En)
}

/// The saved locale, if one was chosen.
pub fn saved_locale(home: &std::path::Path) -> Option<Locale> {
    crate::ui::config::read(home, LOCALE_KEY)?
        .as_str()
        .and_then(Locale::from_code)
}

/// The interface language: the saved locale, else the system's.
pub fn interface_locale(home: &std::path::Path) -> Locale {
    saved_locale(home).unwrap_or_else(system_locale)
}

thread_local! {
    static ACTIVE: Cell<Locale> = const { Cell::new(Locale::En) };
}

pub fn set(locale: Locale) {
    ACTIVE.with(|active| active.set(locale));
}

pub fn active() -> Locale {
    ACTIVE.with(Cell::get)
}

/// `english` in the active locale.
pub fn t(english: &str) -> &str {
    t_in(active(), english)
}

/// `english` in `locale`, without the interface thread's active locale: for a
/// task that may run on another thread and carries the interface language as
/// a value instead.
pub fn t_in(locale: Locale, english: &str) -> &str {
    let lookup = match locale {
        Locale::En => return english,
        Locale::ZhCn => zh_cn::lookup,
        Locale::Ja => ja::lookup,
        Locale::Ko => ko::lookup,
        Locale::ZhHant => zh_hant::lookup,
        Locale::Id => id::lookup,
        Locale::De => de::lookup,
        Locale::Fr => fr::lookup,
        Locale::Es => es::lookup,
        Locale::PtBr => pt_br::lookup,
        Locale::PtPt => pt_pt::lookup,
    };
    lookup(english).unwrap_or(english)
}

/// `english` in the active locale with each `{name}` replaced by its value.
pub fn tf(english: &'static str, args: &[(&str, &str)]) -> String {
    tf_in(active(), english, args)
}

/// [`tf`] in `locale` instead of the active one.
pub fn tf_in(locale: Locale, english: &'static str, args: &[(&str, &str)]) -> String {
    let mut text = t_in(locale, english).to_owned();
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The send-mode strings: the queue strip's hint, the two submit-mode
    /// notices and the two send-now notices, in every locale with their
    /// placeholders kept.
    #[test]
    fn every_locale_translates_the_send_mode_strings() {
        let previous = active();
        for (locale, code, _) in LOCALES {
            if locale == Locale::En {
                continue;
            }
            for key in [
                "/queue · Shift+← edit last · empty: {key} steer · Ctrl+Enter send now",
                "Message queued",
                "Ctrl+Enter interrupt and send now: the draft and every held message, as one message",
                "Send now failed: {error}; the draft was kept",
                "Send now cancelled; the draft was kept",
                "Send now is still stopping the run; the draft was kept",
                "Send now is stopping the run; paste again when the draft is sent",
                "Send now is waiting for the insertion before sending",
            ] {
                let translated = table(locale, key);
                assert!(translated.is_some(), "{code} lacks {key:?}");
                assert_ne!(translated.unwrap(), key, "{code} copies {key:?}");
            }
            set(locale);
            assert!(
                tf(
                    "Send now failed: {error}; the draft was kept",
                    &[("error", "fixture-error")]
                )
                .contains("fixture-error"),
                "{code} drops the error"
            );
            assert!(
                tf(
                    "/queue · Shift+← edit last · empty: {key} steer · Ctrl+Enter send now",
                    &[("key", "Ctrl+S")]
                )
                .contains("Ctrl+S"),
                "{code} drops the key"
            );
        }
        set(previous);
    }

    #[test]
    fn every_locale_translates_the_prompt_suggestion_strings() {
        let previous = active();
        for (locale, code, _) in LOCALES {
            if locale == Locale::En {
                continue;
            }
            set(locale);
            for key in [
                "Prompt suggestion",
                "Prompt suggestion: {error}",
                "Suggestions are paused in this session because offers went unused. Turn them off and on in /settings to resume.",
                "Enter select + default · ←/→ effort · Tab section · Esc clear/close",
                "Default for new sessions in {folder}: {model}",
                "Default for new sessions: {model}",
                "Hook context · {count} line",
                "Hook context · {count} lines",
                "Stop hook follow-up · {count} line",
                "Stop hook follow-up · {count} lines",
            ] {
                assert_ne!(t(key), key, "{code} lacks {key:?}");
            }
            assert!(
                tf("Prompt suggestion: {error}", &[("error", "fixture-error")])
                    .contains("fixture-error"),
                "{code} drops the error"
            );
            for (key, args) in [
                (
                    "Default for new sessions in {folder}: {model}",
                    vec![("folder", "fixture-folder"), ("model", "fixture-model")],
                ),
                (
                    "Default for new sessions: {model}",
                    vec![("model", "fixture-model")],
                ),
            ] {
                let text = tf(key, &args);
                for (_, value) in args {
                    assert!(text.contains(value), "{code} drops {value} from {key:?}");
                }
            }
        }
        set(previous);
    }

    /// The two `edit` preview keys and the two keys the scrolling panel adds,
    /// translated in every locale with the tool name literal and every
    /// argument kept.
    #[test]
    fn every_locale_translates_the_edit_preview_strings() {
        let previous = active();
        for (locale, code, _) in LOCALES {
            if locale == Locale::En {
                continue;
            }
            set(locale);
            for (key, count) in [
                ("edit {count} replacement in {path}", "1"),
                ("edit {count} replacements in {path}", "2"),
            ] {
                assert!(table(locale, key).is_some(), "{code} lacks {key:?}");
                let text = tf(key, &[("count", count), ("path", "fixture-path")]);
                assert!(text.contains("fixture-path"), "{code} drops the path");
                assert!(text.contains(count), "{code} drops the count");
                assert!(text.contains("edit"), "{code} translates the tool name");
            }
            for key in ["↑↓ scroll", "replacements {from}-{to} of {count}"] {
                assert!(table(locale, key).is_some(), "{code} lacks {key:?}");
            }
            let text = tf(
                "replacements {from}-{to} of {count}",
                &[("from", "2"), ("to", "9"), ("count", "30")],
            );
            for value in ["2", "9", "30"] {
                assert!(text.contains(value), "{code} drops {value} from the window");
            }
        }
        set(previous);
    }

    /// The locale's own table, without the English fallback `t` applies.
    fn table(locale: Locale, english: &str) -> Option<&'static str> {
        let lookup: fn(&str) -> Option<&'static str> = match locale {
            Locale::En => return None,
            Locale::ZhCn => zh_cn::lookup,
            Locale::Ja => ja::lookup,
            Locale::Ko => ko::lookup,
            Locale::ZhHant => zh_hant::lookup,
            Locale::Id => id::lookup,
            Locale::De => de::lookup,
            Locale::Fr => fr::lookup,
            Locale::Es => es::lookup,
            Locale::PtBr => pt_br::lookup,
            Locale::PtPt => pt_pt::lookup,
        };
        lookup(english)
    }

    /// The identification and prefix strings, translated in every locale with
    /// their arguments kept.
    #[test]
    fn every_locale_translates_the_identification_strings() {
        let previous = active();
        for (locale, code, _) in LOCALES {
            if locale == Locale::En {
                continue;
            }
            for key in [
                "Model id prefix",
                "(none)",
                "identified: {model} at {provider}",
                "inferred from several rows: {model} at {provider}",
                "ambiguous: {candidates}",
                "unidentified",
                "Identified {rule}",
                "from the Provider's own list",
                "from the whole qualified id",
                "by its tokens",
                "by its canonical id or display name",
                "from the custom-provider list",
                "by merging the rows that name it",
                "ambiguous",
                "{field} {value} supplied by {provider}/{model}",
            ] {
                assert!(table(locale, key).is_some(), "{code} lacks {key:?}");
            }
            set(locale);
            for (key, args) in [
                (
                    "identified: {model} at {provider}",
                    vec![("model", "fixture-model"), ("provider", "fixture-provider")],
                ),
                (
                    "inferred from several rows: {model} at {provider}",
                    vec![("model", "fixture-model"), ("provider", "fixture-provider")],
                ),
                (
                    "ambiguous: {candidates}",
                    vec![("candidates", "fixture-candidate")],
                ),
                ("Identified {rule}", vec![("rule", "fixture-rule")]),
                (
                    "{field} {value} supplied by {provider}/{model}",
                    vec![
                        ("field", "fixture-field"),
                        ("value", "fixture-value"),
                        ("provider", "fixture-provider"),
                        ("model", "fixture-model"),
                    ],
                ),
            ] {
                let text = tf(key, &args);
                for (_, value) in args {
                    assert!(text.contains(value), "{code} drops {value} from {key:?}");
                }
            }
        }
        set(previous);
    }

    /// The `/models` sections: their titles, the management rows and the
    /// focus-specific hints, with the counts kept.
    #[test]
    fn every_locale_translates_the_models_section_strings() {
        let previous = active();
        for (locale, code, _) in LOCALES {
            if locale == Locale::En {
                continue;
            }
            for key in [
                "Conversation model",
                "Selected: {model}",
                "Model management",
                "Manage models",
                "1 provider",
                "{count} providers",
                "ready",
                "no models",
                "catalog unreadable",
                "{count} with missing limits",
                "{count} unavailable",
                "{count} disabled",
                "{count} without models",
                "{count} unreadable",
                "{unavailable} unavailable, {disabled} disabled",
                "Enter select + default · ←/→ effort · Tab section · Esc clear/close",
                "Enter change · Tab section · Esc clear/close",
                "Enter manage · Tab section · Esc clear/close",
                "Enter manage · Esc back",
            ] {
                let translated = table(locale, key);
                assert!(translated.is_some(), "{code} lacks {key:?}");
                assert_ne!(translated.unwrap(), key, "{code} copies {key:?}");
            }
            set(locale);
            for (key, args) in [
                ("Selected: {model}", vec![("model", "fixture-model")]),
                ("{count} providers", vec![("count", "2")]),
                ("{count} with missing limits", vec![("count", "2")]),
                ("{count} unavailable", vec![("count", "2")]),
                ("{count} disabled", vec![("count", "2")]),
                ("{count} without models", vec![("count", "2")]),
                ("{count} unreadable", vec![("count", "2")]),
                (
                    "{unavailable} unavailable, {disabled} disabled",
                    vec![("unavailable", "2"), ("disabled", "3")],
                ),
            ] {
                let text = tf(key, &args);
                for (_, value) in args {
                    assert!(text.contains(value), "{code} drops {value} from {key:?}");
                }
            }
        }
        set(previous);
    }

    #[test]
    fn every_locale_translates_the_model_failure_strings() {
        let previous = active();
        for (locale, code, _) in LOCALES {
            if locale == Locale::En {
                continue;
            }
            for kind in dim_agent_types::ModelFailureKind::ALL {
                for english in [kind.meaning(), kind.action()] {
                    let translated = table(locale, english);
                    assert!(translated.is_some(), "{code} lacks {english:?}");
                    assert_ne!(translated.unwrap(), english, "{code} copies {english:?}");
                }
            }
            for key in [
                "The response above is incomplete.",
                "retrying {attempt}/{max} in {seconds} s: {reason}",
                "Retrying {attempt}/{max} in {seconds} s · {reason} · Esc stops",
                "kind: {kind}",
                "status: {status}",
                "provider code: {code}",
                "request id: {id}",
                "tried {count} times over {seconds} s",
                "Tried {count} times over {seconds} s.",
                "the provider asked to wait {seconds} s",
                "Waiting for the provider · {reason} · Esc stops",
                "cause:",
            ] {
                assert!(table(locale, key).is_some(), "{code} lacks {key:?}");
            }
            set(locale);
            let placeholders: [(&str, &[(&str, &str)]); 10] = [
                ("kind: {kind}", &[("kind", "fixture-kind")]),
                ("status: {status}", &[("status", "503")]),
                ("provider code: {code}", &[("code", "fixture-code")]),
                ("request id: {id}", &[("id", "fixture-request")]),
                (
                    "tried {count} times over {seconds} s",
                    &[("count", "3"), ("seconds", "4.2")],
                ),
                (
                    "the provider asked to wait {seconds} s",
                    &[("seconds", "60")],
                ),
                (
                    "retrying {attempt}/{max} in {seconds} s: {reason}",
                    &[
                        ("attempt", "3"),
                        ("max", "8"),
                        ("seconds", "4"),
                        ("reason", "fixture-reason"),
                    ],
                ),
                (
                    "Retrying {attempt}/{max} in {seconds} s · {reason} · Esc stops",
                    &[
                        ("attempt", "3"),
                        ("max", "8"),
                        ("seconds", "4"),
                        ("reason", "fixture-reason"),
                    ],
                ),
                (
                    "Tried {count} times over {seconds} s.",
                    &[("count", "3"), ("seconds", "4")],
                ),
                (
                    "Waiting for the provider · {reason} · Esc stops",
                    &[("reason", "fixture-reason")],
                ),
            ];
            for (key, args) in placeholders {
                let text = tf(key, args);
                for (_, value) in args {
                    assert!(text.contains(value), "{code} drops {value} from {key:?}");
                }
            }
        }
        set(previous);
    }

    /// The update dialog's and `/update`'s strings, translated in every locale
    /// with their placeholders kept.
    #[test]
    fn every_locale_translates_the_update_strings() {
        let previous = active();
        for (locale, code, _) in LOCALES {
            if locale == Locale::En {
                continue;
            }
            for key in [
                "check for a newer version",
                "Update now",
                "To update, exit DimCode and run: {command}",
                "Finish or stop the current work first (a running turn, shell command, or pending prompt).",
                "Waiting for this Session's state after the switch.",
                "Waiting for the Session switch to finish.",
                "Checking for updates…",
                "DimCode {version} is up to date.",
            ] {
                let translated = table(locale, key);
                assert!(translated.is_some(), "{code} lacks {key:?}");
                assert_ne!(translated.unwrap(), key, "{code} copies {key:?}");
            }
            set(locale);
            for (key, name) in [
                ("To update, exit DimCode and run: {command}", "command"),
                ("DimCode {version} is up to date.", "version"),
            ] {
                let text = tf(key, &[(name, "fixture-value")]);
                assert!(text.contains("fixture-value"), "{code} drops {name}");
            }
        }
        set(previous);
    }

    #[test]
    fn every_locale_translates_the_task_detail_close_hints() {
        for (locale, code, _) in LOCALES {
            if locale == Locale::En {
                continue;
            }
            for key in [
                "Esc close",
                "x stop command · Esc close",
                "x stop workflow · Esc close",
            ] {
                let translated = table(locale, key);
                assert!(translated.is_some(), "{code} lacks {key:?}");
                assert_ne!(translated.unwrap(), key, "{code} copies {key:?}");
            }
        }
    }

    #[test]
    fn every_locale_translates_the_daemon_notice_strings() {
        // The strings the notice prints, so a changed text cannot pass here
        // while every locale silently falls back to English.
        const PLAIN: &str = crate::DAEMON_BUSY;
        const COUNTS: &str = crate::DAEMON_BUSY_COUNTS;
        const TOO_OLD: &str = crate::DAEMON_TOO_OLD;
        const TOO_OLD_COUNTS: &str = crate::DAEMON_TOO_OLD_COUNTS;
        const BUILD_DIFFERENT: &str = crate::DAEMON_BUILD_DIFFERENT;
        const BUILD_UNVERIFIED: &str = crate::DAEMON_BUILD_UNVERIFIED;
        const UPGRADING: &str = crate::DAEMON_UPGRADING;
        const UPGRADING_PROGRESS: &str = crate::DAEMON_UPGRADING_PROGRESS;
        let previous = active();
        for (locale, code, _) in LOCALES {
            if locale == Locale::En {
                continue;
            }
            for key in [
                PLAIN,
                COUNTS,
                TOO_OLD,
                TOO_OLD_COUNTS,
                BUILD_DIFFERENT,
                BUILD_UNVERIFIED,
                UPGRADING,
                UPGRADING_PROGRESS,
                crate::UPGRADE_STAGE_LINES[0],
                crate::UPGRADE_STAGE_LINES[1],
                crate::UPGRADE_STAGE_LINES[2],
                crate::ui::sessions_panel::HINT,
                "Delete \"{name}\"?",
                "Deleted {name}.",
                "Nothing was removed: no stored Session {name}.",
                "Delete failed",
                crate::session_command::DELETE_UNSUPPORTED,
            ] {
                let translated = table(locale, key);
                assert!(translated.is_some(), "{code} lacks {key:?}");
                assert_ne!(translated.unwrap(), key, "{code} copies {key:?}");
            }
            set(locale);
            for (key, args) in [
                (
                    PLAIN,
                    vec![("server", "9.9.9-old"), ("client", "9.9.9-new")],
                ),
                (
                    COUNTS,
                    vec![
                        ("server", "9.9.9-old"),
                        ("client", "9.9.9-new"),
                        ("connections", "2"),
                        ("attached", "1"),
                        ("running", "1"),
                    ],
                ),
                (
                    TOO_OLD,
                    vec![
                        ("server", "9.9.9-old"),
                        ("client", "9.9.9-new"),
                        ("protocol", "3"),
                    ],
                ),
                (
                    TOO_OLD_COUNTS,
                    vec![
                        ("server", "9.9.9-old"),
                        ("client", "9.9.9-new"),
                        ("protocol", "3"),
                        ("connections", "2"),
                        ("attached", "1"),
                        ("running", "1"),
                    ],
                ),
                (UPGRADING, vec![("from", "33"), ("to", "34")]),
                (
                    BUILD_DIFFERENT,
                    vec![
                        ("server", "9.9.9"),
                        ("command", "fixture-client daemon stop"),
                    ],
                ),
                (
                    BUILD_UNVERIFIED,
                    vec![("command", "fixture-client daemon stop")],
                ),
                (
                    UPGRADING_PROGRESS,
                    vec![("seconds", "12"), ("stage", "fixture-stage")],
                ),
                ("Delete \"{name}\"?", vec![("name", "fixture-name")]),
                ("Deleted {name}.", vec![("name", "fixture-name")]),
                (
                    "Nothing was removed: no stored Session {name}.",
                    vec![("name", "fixture-name")],
                ),
            ] {
                let text = tf(key, &args);
                for (_, value) in args {
                    assert!(text.contains(value), "{code} drops {value} from {key:?}");
                }
            }
        }
        set(previous);
    }

    /// Every key the Simplified Chinese areas hold resolves in every locale
    /// through that locale's own `lookup`, so an area dropped from a locale's
    /// chain fails here while its file stays on disk.
    #[test]
    fn every_simplified_chinese_key_resolves_in_every_locale() {
        let mut keys = Vec::new();
        let areas = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui/i18n/zh_cn"));
        for entry in std::fs::read_dir(areas).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|name| name != "mod.rs") {
                let area = table_keys(&std::fs::read_to_string(&path).unwrap());
                assert!(!area.is_empty(), "{}: no key read", path.display());
                keys.extend(area);
            }
        }
        assert!(!keys.is_empty(), "no Simplified Chinese key was read");
        for (locale, code, _) in LOCALES {
            if locale == Locale::En {
                continue;
            }
            for key in &keys {
                assert!(table(locale, key).is_some(), "{code} lacks {key:?}");
            }
        }
    }

    /// The keys one area table tests, before each ` =>`.
    fn table_keys(text: &str) -> Vec<String> {
        text.lines()
            .filter_map(|line| line.trim_start().strip_prefix('"'))
            .filter_map(|line| line.split_once("\" =>").map(|(key, _)| key))
            .map(|key| {
                key.replace("\\\"", "\"")
                    .replace("\\n", "\n")
                    .replace("\\\\", "\\")
            })
            .collect()
    }

    #[test]
    fn environment_values_map_as_the_ts_build_does() {
        let of = Locale::from_env_value;
        assert_eq!(of("zh_CN.UTF-8"), Some(Locale::ZhCn));
        assert_eq!(of("zh_TW.UTF-8"), Some(Locale::ZhHant));
        assert_eq!(of("zh-Hant-HK"), Some(Locale::ZhHant));
        assert_eq!(of("pt_BR"), Some(Locale::PtBr));
        assert_eq!(of("pt"), Some(Locale::PtPt));
        assert_eq!(of("in_ID"), Some(Locale::Id));
        assert_eq!(of("en_US.UTF-8"), Some(Locale::En));
        assert_eq!(of("C"), None);
        assert_eq!(of("POSIX"), None);
    }
}
