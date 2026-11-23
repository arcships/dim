export type AppLocale = "en" | "zh-CN"

export function resolveLocale(language: string): AppLocale {
  return language.toLowerCase().startsWith("zh") ? "zh-CN" : "en"
}
