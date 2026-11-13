<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue"
import { useI18n } from "vue-i18n"
import { useExtensionsStore } from "../../stores/extensions"
import { useUiStore } from "../../stores/ui"
import { useConnectionStore } from "../../stores/connection"
import { readSessionMethods } from "../../rpc/read"
import { panelTitle, panelUnavailable } from "./context"
import { messages } from "./messages"

const props = defineProps<{ sessionId?: string }>()
const { t, locale } = useI18n({ messages })
const extensions = useExtensionsStore()
const ui = useUiStore()
const connection = useConnectionStore()
const methods = ref<string[]>()
const error = ref("")
let request = 0
const panels = computed(() => (extensions.directory?.extensions ?? []).flatMap(extension => extension.panels.map(panel => ({
  extension, panel,
  reason: connection.phase !== "connected" ? t("disconnected")
    : panel.methods.length && methods.value === undefined ? error.value || t("checking")
    : panelUnavailable(panel, connection.supports, methods.value ?? []),
}))).sort((a, b) => a.extension.pluginName.localeCompare(b.extension.pluginName) || a.panel.id.localeCompare(b.panel.id)))
async function refresh(): Promise<void> {
  const current = ++request
  methods.value = undefined
  error.value = ""
  if (!props.sessionId || connection.phase !== "connected" || !panels.value.some(({ panel }) => panel.methods.length)) return
  try {
    const result = await connection.client.call("session.methods", { sessionId: props.sessionId }, readSessionMethods)
    if (current === request) methods.value = result
  } catch (reason) { if (current === request) error.value = reason instanceof Error ? reason.message : String(reason) }
}
watch([() => props.sessionId, () => connection.phase, () => extensions.directory?.revision], refresh, { immediate: true })
onBeforeUnmount(() => { request++ })
function select(value: unknown): void {
  if (typeof value !== "string" || value === "") return
  const entry = panels.value.find(entry => JSON.stringify([entry.extension.pluginName, entry.panel.id]) === value)
  if (entry === undefined || entry.reason) return
  ui.extension = { pluginName: entry.extension.pluginName, panelId: entry.panel.id }
  ui.detailExpanded = false
  ui.detailOpen = true
  ui.detailFocused = true
}
const selectedValue = computed(() => ui.extension && ui.detailOpen ? JSON.stringify([ui.extension.pluginName, ui.extension.panelId]) : undefined)
const menuItems = computed(() => [
  ...panels.value.map(({ extension, panel, reason }) => ({
    label: `${panelTitle(panel, locale.value)} · ${extension.pluginName}${reason ? ` — ${reason}` : ""}`,
    value: JSON.stringify([extension.pluginName, panel.id]),
    disabled: Boolean(reason),
  })),
  ...(extensions.directory?.errors ?? []).map(err => ({
    label: `${err.pluginName}: ${err.message}`,
    value: `__error__:${err.pluginName}`,
    disabled: true,
  })),
])
</script>

<template>
  <div class="extension-picker">
    <USelect
      data-extension-trigger
      size="sm"
      class="extension-select"
      :model-value="selectedValue"
      :items="menuItems"
      :placeholder="panels.length ? t('choose') : t('empty')"
      :aria-label="t('extensions')"
      :title="sessionId ? t('choose') : t('session')"
      :disabled="!sessionId || !menuItems.length"
      @update:open="$event && refresh()"
      @update:model-value="select"
    />
    <span v-if="extensions.error" role="status">{{ extensions.error }}</span>
  </div>
</template>

<style scoped>
.extension-picker { min-width: 0; flex: 0 1 auto; }
.extension-select { width: clamp(8rem, 18vw, 16rem); max-width: 100%; }
</style>
