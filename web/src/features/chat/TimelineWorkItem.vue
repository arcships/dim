<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from "vue"
import { Check, ChevronRight, Copy } from "lucide-vue-next"
import { useI18n } from "vue-i18n"
import { copyText } from "../../clipboard"
import { diffLines, resolveToolDisplay } from "./tool-display"
import { formatElapsedSeconds } from "./timeline-time"
import type { WorkItem } from "./work"

// Panels scroll at 240px; the cap only keeps a huge result from flooding the DOM.
const MAX_PANEL_CHARS = 20_000
const props = defineProps<{ item: WorkItem; expanded: boolean }>()
const emit = defineEmits<{ toggle: [] }>()
const { t } = useI18n()

const tool = computed(() => props.item.kind === "tool" ? props.item : undefined)
const display = computed(() => tool.value === undefined ? undefined : resolveToolDisplay(tool.value.name, tool.value.input))
const label = computed(() => {
  const current = display.value
  if (current === undefined || tool.value === undefined) return ""
  return current.renderer === "generic" ? tool.value.name : t(current.labelKey)
})

const thinkingTitle = computed(() => {
  if (props.item.kind !== "thinking") return ""
  if (props.item.running) return t("work.thinking")
  return t("work.thoughtFor", { duration: props.item.durationMs === undefined ? "…" : formatElapsedSeconds(props.item.durationMs) })
})
const thinkingText = computed(() => props.item.kind === "thinking" ? props.item.content.trim() : "")

function clip(text: string): string {
  return text.length <= MAX_PANEL_CHARS ? text : `${text.slice(0, MAX_PANEL_CHARS)}…`
}

function inputString(key: string): string | undefined {
  const value = tool.value?.input[key]
  return typeof value === "string" && value !== "" ? value : undefined
}

const output = computed(() => clip((tool.value?.result?.text ?? "").replace(/\s+$/u, "")))
const isError = computed(() => tool.value?.status === "error")
const name = computed(() => tool.value?.name.trim().toLowerCase() ?? "")
const diff = computed(() => name.value === "edit" ? diffLines(tool.value?.input ?? {}, tool.value?.result?.diff) : [])
const written = computed(() => name.value === "write" ? clip(inputString("content") ?? "") : "")

/** An edit's `+N −N` line counts. */
const stats = computed(() => {
  if (diff.value.length > 0) return { added: diff.value.filter((line) => line.sign === "+").length, removed: diff.value.filter((line) => line.sign === "-").length }
  if (written.value !== "") return { added: written.value.split("\n").length, removed: 0 }
  return undefined
})

type PanelSection = { id: string; label: string; copy: string; body: "command" | "search" | "diff" | "lines" | "pre" }

/** The code panel's sections, each under a label bar with its own copy action. */
const sections = computed<PanelSection[]>(() => {
  const current = tool.value
  if (current === undefined || display.value === undefined) return []
  const outputSection: PanelSection[] = output.value ? [{ id: "output", label: t("detail.output"), copy: output.value, body: "pre" }] : []
  switch (display.value.renderer) {
    case "command": {
      const command = inputString("command")
      return [...(command === undefined ? [] : [{ id: "input", label: t("prompt.input"), copy: command, body: "command" as const }]), ...outputSection]
    }
    case "search": {
      const pattern = inputString("pattern") ?? inputString("path")
      return [...(pattern === undefined ? [] : [{ id: "input", label: t("prompt.input"), copy: pattern, body: "search" as const }]), ...outputSection]
    }
    case "file": {
      // A read opens nothing inline but its error.
      const path = inputString("path") ?? current.name
      if (diff.value.length > 0) return [{ id: "file", label: path, copy: diff.value.map((line) => `${line.sign}${line.text}`).join("\n"), body: "diff" }, ...(isError.value ? outputSection : [])]
      if (written.value !== "") return [{ id: "file", label: path, copy: written.value, body: "lines" }, ...(isError.value ? outputSection : [])]
      return isError.value ? outputSection : []
    }
    default: {
      const input = Object.keys(current.input).length === 0 ? "" : clip(JSON.stringify(current.input, null, 2))
      return [...(input ? [{ id: "input", label: t("prompt.input"), copy: input, body: "pre" as const }] : []), ...outputSection]
    }
  }
})

const hasDetails = computed(() => props.item.kind === "thinking" ? thinkingText.value !== "" : sections.value.length > 0)

const copiedId = ref<string>()
let copiedTimer: ReturnType<typeof setTimeout> | undefined
onBeforeUnmount(() => clearTimeout(copiedTimer))

async function copy(section: PanelSection): Promise<void> {
  await copyText(section.copy)
  copiedId.value = section.id
  clearTimeout(copiedTimer)
  copiedTimer = setTimeout(() => { copiedId.value = undefined }, 1500)
}
</script>

<template>
  <div class="timeline-work-item">
    <button
      type="button"
      class="timeline-work-item-toggle"
      :class="{ 'is-running': item.kind === 'thinking' ? item.running : item.status === 'running', 'is-error': isError, 'is-static': !hasDetails }"
      :aria-expanded="hasDetails ? expanded : undefined"
      @click="hasDetails && emit('toggle')"
    >
      <ChevronRight :size="14" aria-hidden="true" :class="{ 'is-expanded': expanded && hasDetails }" />
      <span v-if="tool" class="timeline-work-item-summary" :title="[label, tool.primary].filter(Boolean).join(' ')">
        <span>{{ label }}</span><small v-if="tool.primary">{{ tool.primary }}</small>
      </span>
      <span v-else class="timeline-work-item-summary"><span>{{ thinkingTitle }}</span></span>
      <template v-if="stats">
        <span class="timeline-work-stat" data-tone="success">+{{ stats.added }}</span>
        <span v-if="stats.removed" class="timeline-work-stat" data-tone="danger">−{{ stats.removed }}</span>
      </template>
    </button>

    <div v-if="expanded && hasDetails" class="timeline-work-item-details">
      <div v-if="item.kind === 'thinking'" class="timeline-thinking-quote">{{ thinkingText }}</div>
      <div v-else class="tool-panel">
        <template v-for="section in sections" :key="section.id">
          <div class="tool-panel-label">
            <span :title="section.label">{{ section.label }}</span>
            <button type="button" class="ui-button" data-variant="ghost" data-size="xs" @click="copy(section)">
              <Check v-if="copiedId === section.id" aria-hidden="true" /><Copy v-else aria-hidden="true" />{{ t(copiedId === section.id ? "timeline.copied" : "timeline.copy") }}
            </button>
          </div>
          <div v-if="section.body === 'command'" class="tool-panel-code">
            <div class="is-strong"><span class="is-muted">$</span> {{ section.copy }}</div>
            <div v-if="inputString('cwd')" class="is-faint">  cwd: {{ inputString("cwd") }}</div>
          </div>
          <div v-else-if="section.body === 'search'" class="tool-panel-code">
            <div class="is-strong">{{ section.copy }}</div>
            <div v-if="inputString('pattern') && inputString('path')" class="is-faint">  path: {{ inputString("path") }}</div>
          </div>
          <div v-else-if="section.body === 'diff'" class="tool-panel-diff">
            <div v-for="(line, index) in diff" :key="index" class="tool-panel-diff-line" :data-sign="line.sign"><span class="tool-panel-sign" aria-hidden="true">{{ line.sign }}</span>{{ line.text }}</div>
          </div>
          <div v-else-if="section.body === 'lines'" class="tool-panel-lines">
            <template v-for="(line, index) in written.split('\n')" :key="index">
              <span class="tool-panel-line-number" aria-hidden="true">{{ index + 1 }}</span><span class="tool-panel-line">{{ line }}</span>
            </template>
          </div>
          <pre v-else class="tool-panel-pre" :class="{ 'is-error': isError && section.id === 'output' }">{{ section.copy }}</pre>
        </template>
      </div>
    </div>
  </div>
</template>

<style scoped>
.timeline-work-stat { flex: none; font-family: var(--font-mono); font-size: var(--text-xs); font-variant-numeric: tabular-nums; }
.timeline-work-stat[data-tone="success"] { color: var(--success); }
.timeline-work-stat[data-tone="danger"] { color: var(--danger); }
/* Code panel: a bordered card that scrolls at 240px; each section opens with a
   sticky muted label bar (mono label, ghost Copy). */
.tool-panel { max-height: 240px; margin-top: var(--space-1); overflow: auto; isolation: isolate; border: 1px solid var(--border); border-radius: var(--radius-lg); background: var(--card); }
.tool-panel-label { position: sticky; z-index: var(--z-sticky); top: 0; display: flex; height: var(--control-md); align-items: center; justify-content: space-between; gap: var(--space-2); padding: 0 var(--space-1) 0 var(--space-3); border-bottom: 1px solid var(--border); background: var(--muted); color: var(--muted-foreground); }
.tool-panel-label:not(:first-child) { border-top: 1px solid var(--border); }
.tool-panel-label > span { min-width: 0; overflow: hidden; font-family: var(--font-mono); font-size: var(--text-xs); text-overflow: ellipsis; white-space: nowrap; }
.tool-panel-code, .tool-panel-pre { margin: 0; padding: var(--space-2) var(--space-3); font: var(--text-sm)/var(--leading-mono) var(--font-mono); white-space: pre-wrap; overflow-wrap: anywhere; }
.tool-panel-code .is-strong { color: color-mix(in oklab, var(--foreground) 90%, transparent); }
.tool-panel-code .is-muted { color: var(--muted-foreground); }
.tool-panel-code .is-faint { color: color-mix(in oklab, var(--muted-foreground) 70%, transparent); }
.tool-panel-pre { color: color-mix(in oklab, var(--foreground) 80%, transparent); }
.tool-panel-pre.is-error { color: var(--danger); }
.tool-panel-diff { padding: var(--space-2) 0; font: var(--text-sm)/var(--leading-mono) var(--font-mono); }
.tool-panel-diff-line { padding: 0 var(--space-3); white-space: pre-wrap; overflow-wrap: anywhere; }
.tool-panel-diff-line[data-sign="-"] { background: color-mix(in oklab, var(--danger) 8%, transparent); color: color-mix(in oklab, var(--danger) 72%, var(--foreground)); }
.tool-panel-diff-line[data-sign="+"] { background: color-mix(in oklab, var(--success) 10%, transparent); color: color-mix(in oklab, var(--success) 66%, var(--foreground)); }
.tool-panel-sign { display: inline-block; width: 2ch; user-select: none; }
.tool-panel-lines { display: grid; grid-template-columns: max-content minmax(0, 1fr); column-gap: var(--space-3); padding: var(--space-2) var(--space-3); font: var(--text-sm)/var(--leading-mono) var(--font-mono); }
.tool-panel-line-number { color: color-mix(in oklab, var(--muted-foreground) 60%, transparent); text-align: right; user-select: none; }
.tool-panel-line { color: color-mix(in oklab, var(--foreground) 80%, transparent); white-space: pre-wrap; overflow-wrap: anywhere; }
</style>
