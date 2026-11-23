import { compile, createCoreContext, registerMessageCompiler, translate as formatMessage } from "@intlify/core-base"
import { ref } from "@vue/reactivity"
import en, { type Messages, type TranslationKey } from "../i18n/en"
import { resolveLocale, type AppLocale } from "../i18n/locale"

export const appLocale = ref<AppLocale>("en")
const messages: Partial<Record<AppLocale, Messages>> = { en }
registerMessageCompiler(compile)
const context = createCoreContext({ locale: appLocale.value, fallbackLocale: "en", messages })

export async function setAppLocale(language: string): Promise<void> {
  const locale = resolveLocale(language)
  if (locale === "zh-CN" && messages[locale] === undefined) messages[locale] = (await import("../i18n/zh-CN")).default
  context.locale = locale
  appLocale.value = locale
  document.documentElement.lang = locale
}

export function translate(key: TranslationKey, params: Record<string, string | number> = {}): string {
  return String(formatMessage(context, key, params))
}
