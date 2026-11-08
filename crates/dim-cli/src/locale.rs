//! The interface language's identity: the locales the CLI offers, their codes
//! and labels, and how an environment value names one. The translation tables
//! and the interface thread's active locale stay in `ui::i18n`; this module
//! sits below both clients so a shared module can carry a language as a
//! value, for a fiber task that may run on another thread.

/// One interface language.
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
