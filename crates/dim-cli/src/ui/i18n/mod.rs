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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    En,
    ZhCn,
    Ja,
    Ko,
    ZhHant,
    Id,
    De,
    Fr,
    Es,
    PtBr,
    PtPt,
}

/// Every locale in picker order: `(locale, code, label)`.
pub const LOCALES: [(Locale, &str, &str); 11] = [
    (Locale::En, "en", "English"),
    (Locale::ZhCn, "zh-CN", "简体中文"),
    (Locale::Ja, "ja", "日本語"),
    (Locale::Ko, "ko", "한국어"),
    (Locale::ZhHant, "zh-Hant", "繁體中文"),
    (Locale::Id, "id", "Bahasa Indonesia"),
    (Locale::De, "de", "Deutsch"),
    (Locale::Fr, "fr", "Français"),
    (Locale::Es, "es", "Español"),
    (Locale::PtBr, "pt-BR", "Português (Brasil)"),
    (Locale::PtPt, "pt-PT", "Português (Portugal)"),
];

/// `config.json` key of the saved locale; absent means the system default.
pub const LOCALE_KEY: &str = "locale";

impl Locale {
    fn entry(self) -> &'static (Locale, &'static str, &'static str) {
        LOCALES
            .iter()
            .find(|(l, ..)| *l == self)
            .unwrap_or(&LOCALES[0])
    }

    pub fn code(self) -> &'static str {
        self.entry().1
    }

    pub fn label(self) -> &'static str {
        self.entry().2
    }

    pub fn from_code(code: &str) -> Option<Self> {
        LOCALES
            .iter()
            .find(|(_, c, _)| *c == code)
            .map(|(l, ..)| *l)
    }

    /// A locale from an environment value such as `zh_CN.UTF-8` (the TS
    /// `normalizeLocale`); `C`, `POSIX` and unknown languages give none.
    pub fn from_env_value(value: &str) -> Option<Self> {
        let base = value.split('.').next()?.replace('_', "-").to_lowercase();
        let lang = base.split('-').next()?;
        Some(match (lang, base.as_str()) {
            ("en", _) => Self::En,
            ("zh", "zh-tw" | "zh-hk" | "zh-mo") => Self::ZhHant,
            ("zh", b) if b.starts_with("zh-hant") => Self::ZhHant,
            ("zh", _) => Self::ZhCn,
            ("ja", _) => Self::Ja,
            ("ko", _) => Self::Ko,
            ("id" | "in", _) => Self::Id,
            ("de", _) => Self::De,
            ("fr", _) => Self::Fr,
            ("es", _) => Self::Es,
            ("pt", "pt-br") => Self::PtBr,
            ("pt", _) => Self::PtPt,
            _ => return None,
        })
    }
}

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
pub fn t(english: &'static str) -> &'static str {
    let lookup = match active() {
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
    let mut text = t(english).to_owned();
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
