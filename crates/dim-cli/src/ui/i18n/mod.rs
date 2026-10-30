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

    type Lookup = fn(&str) -> Option<&'static str>;

    fn tables() -> [(&'static str, Lookup); 10] {
        let tables = [
            ("zh-CN", zh_cn::lookup as Lookup),
            ("ja", ja::lookup),
            ("ko", ko::lookup),
            ("zh-Hant", zh_hant::lookup),
            ("id", id::lookup),
            ("de", de::lookup),
            ("fr", fr::lookup),
            ("es", es::lookup),
            ("pt-BR", pt_br::lookup),
            ("pt-PT", pt_pt::lookup),
        ];
        assert_eq!(tables.len() + 1, LOCALES.len());
        tables
    }

    /// Every string `/plugins` draws has a translation in every locale.
    #[test]
    fn every_locale_translates_the_plugins_strings() {
        for (code, lookup) in tables() {
            for key in crate::ui::plugins_panel::STRINGS {
                assert!(lookup(key).is_some(), "{code} lacks {key:?}");
            }
        }
    }

    /// `/settings` names command isolation.
    #[test]
    fn every_locale_translates_command_isolation() {
        for (code, lookup) in tables() {
            assert!(
                lookup("Command isolation").is_some(),
                "{code} lacks Command isolation"
            );
        }
    }

    /// The approval prompt and the transcript command row name the requested
    /// boundary; a table that drops the placeholder would print the tier name
    /// nowhere.
    #[test]
    fn every_locale_translates_the_sandbox_boundary_and_keeps_its_placeholder() {
        for (code, lookup) in tables() {
            for key in ["boundary: {sandbox}", " (boundary: {sandbox})"] {
                let text = lookup(key).unwrap_or_else(|| panic!("{code} lacks {key:?}"));
                assert!(
                    text.contains("{sandbox}"),
                    "{code} drops the placeholder in {key:?}: {text:?}"
                );
            }
        }
    }

    /// The compaction row, its `/compact` reply and the gauge's warning are
    /// translated in every locale.
    #[test]
    fn every_locale_translates_the_compaction_strings() {
        for (code, lookup) in tables() {
            for key in [
                "Compacting context…",
                "Context compacted",
                "Context compaction skipped",
                "Context compaction failed",
                "Context compaction cancelled",
                "near the compaction threshold",
                "This daemon does not support context compaction",
                "compact the context of the shown line",
                "Kept verbatim",
                "Tool effects",
                "insufficient space: {parts}",
                "system prompt",
                "tools",
                "appended messages",
                "keyed baseline",
                "pinned inputs",
                "digest",
                "summary",
            ] {
                assert!(lookup(key).is_some(), "{code} lacks {key:?}");
            }
        }
    }

    /// A delegation the server refused before a member existed reads as a
    /// rejected attempt, in every locale; a table that drops a placeholder
    /// would print the refusal's reason or the count nowhere.
    #[test]
    fn every_locale_translates_the_rejected_delegation_strings() {
        for (code, lookup) in tables() {
            for key in [
                "Not started",
                "Reason: {reason}",
                "1 delegation rejected",
                "{count} delegations rejected",
                " · {count} rejected",
            ] {
                assert!(lookup(key).is_some(), "{code} lacks {key:?}");
            }
            for (key, placeholder) in [
                ("Reason: {reason}", "{reason}"),
                ("{count} delegations rejected", "{count}"),
                (" · {count} rejected", "{count}"),
            ] {
                let text = lookup(key).unwrap();
                assert!(
                    text.contains(placeholder),
                    "{code} drops {placeholder} in {key:?}: {text:?}"
                );
            }
        }
    }

    /// A provider that offers no selectable model keeps a row in the models
    /// picker saying why, in every locale; a table that drops a placeholder
    /// would print the provider, its count or the explaining command nowhere.
    #[test]
    fn every_locale_translates_the_models_picker_provider_notice() {
        for (code, lookup) in tables() {
            for (key, placeholders) in [
                (
                    "{provider}: no models · dimcode models {id} --refresh",
                    &["{provider}", "{id}"][..],
                ),
                (
                    "{provider}: no models · add a model with provider.models.create",
                    &["{provider}"][..],
                ),
                (
                    "{provider}: {unavailable} unavailable and {disabled} disabled · dimcode models {id}",
                    &["{provider}", "{unavailable}", "{disabled}", "{id}"][..],
                ),
                (
                    "{provider}: catalog unreadable · dimcode models {id}",
                    &["{provider}", "{id}"][..],
                ),
                (
                    "{provider}: 1 model unavailable · dimcode models {id}",
                    &["{provider}", "{id}"][..],
                ),
                (
                    "{provider}: {count} models unavailable · dimcode models {id}",
                    &["{provider}", "{count}", "{id}"][..],
                ),
                (
                    "{provider}: 1 model disabled · dimcode models {id}",
                    &["{provider}", "{id}"][..],
                ),
                (
                    "{provider}: {count} models disabled · dimcode models {id}",
                    &["{provider}", "{count}", "{id}"][..],
                ),
            ] {
                let text = lookup(key).unwrap_or_else(|| panic!("{code} lacks {key:?}"));
                for placeholder in placeholders {
                    assert!(
                        text.contains(placeholder),
                        "{code} drops {placeholder} in {key:?}: {text:?}"
                    );
                }
            }
        }
    }

    /// `dimcode models PROVIDER` names an empty catalog instead of printing
    /// nothing, in every locale: the cached catalog names the refresh command
    /// and keeps the provider it was asked about, and a catalog just read
    /// from the provider says the listing came back empty.
    #[test]
    fn every_locale_translates_the_empty_model_catalog_notice() {
        for (code, lookup) in tables() {
            let key = "No models. Run dimcode models {id} --refresh to list them.";
            let text = lookup(key).unwrap_or_else(|| panic!("{code} lacks {key:?}"));
            assert!(
                text.contains("{id}"),
                "{code} drops {{id}} in {key:?}: {text:?}"
            );
            let key = "No models. The provider listed none.";
            let text = lookup(key).unwrap_or_else(|| panic!("{code} lacks {key:?}"));
            assert!(
                !text.contains('{'),
                "{code} keeps a placeholder in {key:?}: {text:?}"
            );
            let key = "No cached catalog. Sign in and run models PROVIDER --refresh.";
            let text = lookup(key).unwrap_or_else(|| panic!("{code} lacks {key:?}"));
            assert!(
                !text.contains('{'),
                "{code} keeps a placeholder in {key:?}: {text:?}"
            );
            let key = "Saved model preferences cleared";
            let text = lookup(key).unwrap_or_else(|| panic!("{code} lacks {key:?}"));
            assert!(
                !text.contains('{'),
                "{code} keeps a placeholder in {key:?}: {text:?}"
            );
        }
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
