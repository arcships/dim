import { FieldSelect } from "./field-select"
import { Button } from "@heroui/react"
import { Moon, Sun } from "lucide-react"
import { appLocale } from "./i18n"
import { useComposerStore } from "../stores/composer"
import { appearance, useStoreValue } from "./stores"

const composer = useComposerStore()
const readPreferences = () => ({
  theme: appearance.resolvedTheme,
  fontSize: appearance.fontSize,
  language: appearance.language,
  locale: appLocale.value,
  error: appearance.error || composer.error,
  sendMode: composer.sendMode,
})
const themes = [
  { value: "light", Icon: Sun, en: "Light", zh: "浅色" },
  { value: "dark", Icon: Moon, en: "Dark", zh: "深色" },
] as const

export function AppearanceSettings() {
  const preferences = useStoreValue(readPreferences)
  const text = (en: string, zh: string) => preferences.locale === "zh-CN" ? zh : en

  return <section className="rw-settings">
    <h1>{text("Appearance", "外观")}</h1>
    {preferences.error && <p className="rw-error" role="alert">{preferences.error}</p>}
    <fieldset>
      <legend>{text("Theme", "主题")}</legend>
      <div className="rw-setting-options">
        {themes.map(({ value, Icon, en, zh }) => <Button key={value} variant={preferences.theme === value ? "primary" : "secondary"}
          aria-pressed={preferences.theme === value} onPress={() => { appearance.theme = value }}>
          <Icon size={17} /> {text(en, zh)}
        </Button>)}
      </div>
    </fieldset>
    <fieldset>
      <legend>{text("UI font size", "界面字号")}</legend>
      <div className="rw-setting-options">
        {[12, 13, 14, 15, 16, 18].map((size) => <Button key={size} size="sm" variant={preferences.fontSize === size ? "primary" : "secondary"}
          aria-pressed={preferences.fontSize === size} onPress={() => { appearance.fontSize = size }}>{size}</Button>)}
      </div>
    </fieldset>
    <fieldset>
      <legend>{text("Send during a run", "运行中发送")}</legend>
      <div className="rw-setting-options">
        <Button size="sm" variant={preferences.sendMode === "queue" ? "primary" : "secondary"} aria-pressed={preferences.sendMode === "queue"}
          onPress={() => { composer.sendMode = "queue" }}>{text("Hold for the next turn", "暂存到下一轮")}</Button>
        <Button size="sm" variant={preferences.sendMode === "steer" ? "primary" : "secondary"} aria-pressed={preferences.sendMode === "steer"}
          onPress={() => { composer.sendMode = "steer" }}>{text("Steer into the run", "插话到当前运行")}</Button>
      </div>
    </fieldset>
    <div className="rw-select-field">
      <span>{text("Interface language", "界面语言")}</span>
      <FieldSelect label={text("Interface language", "界面语言")} value={preferences.language}
        onChange={(selectedValue) => {
          const language = selectedValue
          if (language === "" || language === "en" || language === "zh-CN") void appearance.selectLanguage(language)
        }} options={[
          { value: "", label: text("System default", "跟随系统") },
          { value: "en", label: "English" },
          { value: "zh-CN", label: "简体中文" },
        ]} />
    </div>
  </section>
}
