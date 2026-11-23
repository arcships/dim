import { ref, watch } from "@vue/reactivity"
import { defineStore } from "#workspace-store"
import { setAppLocale } from "#workspace-i18n"

type Theme = "light" | "dark" | "system"
type Language = "" | "en" | "zh-CN"
const key = "dim-web.appearance.v1"

export const useAppearanceStore = defineStore("appearance", () => {
  const theme = ref<Theme>("system")
  const fontSize = ref(14)
  const language = ref<Language>("")
  const error = ref("")
  try {
    const value: unknown = JSON.parse(localStorage.getItem(key) ?? "null")
    if (value !== null && typeof value === "object" && !Array.isArray(value)) {
      const stored = value as Record<string, unknown>
      if (stored.theme === "light" || stored.theme === "dark" || stored.theme === "system") theme.value = stored.theme
      if (typeof stored.fontSize === "number" && [12, 13, 14, 15, 16, 18].includes(stored.fontSize)) fontSize.value = stored.fontSize
      if (stored.language === "en" || stored.language === "zh-CN") language.value = stored.language
    }
  } catch { error.value = "Browser appearance preferences could not be read." }

  const resolvedTheme = ref<"light" | "dark">("light")
  const systemDark = matchMedia("(prefers-color-scheme: dark)")
  function apply(): void {
    resolvedTheme.value = theme.value === "dark" || (theme.value === "system" && systemDark.matches) ? "dark" : "light"
    document.documentElement.classList.toggle("dark", resolvedTheme.value === "dark")
    document.documentElement.style.setProperty("--ui-font-size", String(fontSize.value))
  }
  apply()
  systemDark.addEventListener("change", apply)
  watch([theme, fontSize, language], () => {
    apply()
    error.value = ""
    try { localStorage.setItem(key, JSON.stringify({ theme: theme.value, fontSize: fontSize.value, language: language.value })) }
    catch { error.value = "Browser storage is unavailable. Appearance changes will not survive a reload." }
  })

  async function selectLanguage(value: Language): Promise<void> {
    try {
      await setAppLocale(value || navigator.language)
      language.value = value
    } catch { error.value = "The selected language could not be loaded." }
  }
  return { theme, resolvedTheme, fontSize, language, selectLanguage, error }
})
