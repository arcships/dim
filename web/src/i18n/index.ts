import { createI18n } from "vue-i18n"
import en, { type Messages, type TranslationKey } from "./en"

export type AppLocale = "en" | "zh-CN"

/** English ships in the entry chunk; Chinese is fetched when the browser asks for it. */
const loadZh = (): Promise<{ default: Messages }> => import("./zh-CN")

export const i18n = createI18n({
  legacy: false,
  locale: "en",
  fallbackLocale: "en",
  messages: { en } as Record<AppLocale, Messages>,
})

export function resolveLocale(language: string): AppLocale {
  return language.toLowerCase().startsWith("zh") ? "zh-CN" : "en"
}

/** Loads the browser language's messages once, then switches to it. */
export async function setAppLocale(language: string): Promise<void> {
  const locale = resolveLocale(language)
  if (locale === "zh-CN" && !i18n.global.availableLocales.includes(locale)) {
    i18n.global.setLocaleMessage(locale, (await loadZh()).default)
  }
  i18n.global.locale.value = locale
}

export function translate(key: TranslationKey, params?: Record<string, string | number>): string {
  return i18n.global.t(key, params ?? {})
}

export type { TranslationKey }
