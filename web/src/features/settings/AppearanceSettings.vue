<script setup lang="ts">
import { computed } from "vue"
import { useSettingsText } from "../../i18n/settings-general"
import { useAppearanceStore } from "../../stores/appearance"

const s = useSettingsText()
const appearance = useAppearanceStore()
const themes = [{ value: 'light', icon: 'i-lucide-sun' }, { value: 'dark', icon: 'i-lucide-moon' }, { value: 'system', icon: 'i-lucide-monitor' }] as const
const languageItems = computed(() => [
  { label: s('System default', '跟随系统'), value: '__system__' },
  { label: 'English', value: 'en' },
  { label: '简体中文', value: 'zh-CN' },
])
function selectLanguage(value: string): void {
  const next = value === "__system__" ? "" : value
  if (next === "" || next === "en" || next === "zh-CN") void appearance.selectLanguage(next)
}
</script>

<template>
  <article class="settings-page">
    <header><h2 data-route-heading tabindex="-1">{{ s('Appearance', '外观') }}</h2><p>{{ s('Changes apply immediately and are saved in this browser, independently of the CLI.', '更改立即生效并保存在此浏览器中，与 CLI 设置相互独立。') }}</p></header>
    <UAlert v-if="appearance.error" class="settings-banner" color="error" variant="subtle" :description="appearance.error" role="alert" />
    <section class="settings-card"><h3>{{ s('Theme', '主题') }}</h3><div class="theme-options" role="group" :aria-label="s('Theme', '主题')"><button v-for="theme in themes" :key="theme.value" class="theme-option" :class="{ 'is-selected': appearance.theme === theme.value }" type="button" :aria-pressed="appearance.theme === theme.value" @click="appearance.theme = theme.value"><UIcon :name="theme.icon" class="size-[22px]" aria-hidden="true" /><span>{{ theme.value === 'light' ? s('Light', '浅色') : theme.value === 'dark' ? s('Dark', '深色') : s('System', '跟随系统') }}</span></button></div></section>
    <section class="settings-card"><h3>{{ s('UI font size', '界面字号') }}</h3><div class="settings-actions" role="group" :aria-label="s('UI font size', '界面字号')"><UButton v-for="size in [12, 13, 14, 15, 16, 18]" :key="size" size="md" :color="appearance.fontSize === size ? 'primary' : 'neutral'" :variant="appearance.fontSize === size ? 'solid' : 'outline'" :aria-pressed="appearance.fontSize === size" @click="appearance.fontSize = size">{{ size }}</UButton></div><p>{{ s('Scales the entire interface, including controls and chat text.', '缩放整个界面，包括控件和聊天文本。') }}</p></section>
    <section class="settings-card"><h3>{{ s('Language', '语言') }}</h3><UFormField :label="s('Interface language', '界面语言')"><USelect :model-value="appearance.language || '__system__'" :items="languageItems" size="md" class="w-full" @update:model-value="selectLanguage" /></UFormField></section>
  </article>
</template>

<style scoped>
.theme-options { display: grid; grid-template-columns: repeat(3, 1fr); gap: var(--space-3); }
.theme-option { display: flex; min-width: 0; flex-direction: column; align-items: center; gap: var(--space-3); padding: var(--space-5) var(--space-2); border: 1px solid var(--border); border-radius: var(--radius-lg); background: transparent; color: var(--foreground); font-size: var(--text-md); cursor: pointer; }
.theme-option:hover { background: var(--hover); }
.theme-option.is-selected { border-color: var(--primary); background: var(--primary-soft); color: var(--primary); }
.theme-option:focus-visible { outline: 2px solid var(--primary); outline-offset: 3px; }
</style>
