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

    #[test]
    fn every_locale_translates_the_daemon_notice_strings() {
        // The strings the notice prints, so a changed text cannot pass here
        // while every locale silently falls back to English.
        const PLAIN: &str = crate::DAEMON_BUSY;
        const COUNTS: &str = crate::DAEMON_BUSY_COUNTS;
        const UPGRADING: &str = crate::DAEMON_UPGRADING;
        let previous = active();
        for (locale, code, _) in LOCALES {
            if locale == Locale::En {
                continue;
            }
            for key in [
                PLAIN,
                COUNTS,
                UPGRADING,
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
                (UPGRADING, vec![("from", "33"), ("to", "34")]),
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
