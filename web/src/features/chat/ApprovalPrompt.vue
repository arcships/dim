<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, type Component } from "vue"
import { useI18n } from "vue-i18n"
import { FilePen, Globe, Search, Terminal, Wrench } from "lucide-vue-next"
import type { ApprovalDecision, ApprovalRequest } from "../../rpc/protocol"
import { useChatStore } from "../../stores/chat"
import { diffLines, resolveToolDisplay, type ToolRenderer } from "./tool-display"

/**
 * The tool approval prompt: it takes the composer's place, names the action,
 * shows the call's input, and answers Deny or Allow (Enter/y, Escape/n).
 */
const props = defineProps<{ sessionId: string; request: ApprovalRequest }>()
const { t } = useI18n()
const chat = useChatStore()
const pending = ref(false)
const error = ref("")

const input = computed(() => props.request.call.input)
const display = computed(() => resolveToolDisplay(props.request.call.name, input.value))
const ICONS: Record<ToolRenderer, Component> = { command: Terminal, search: Search, file: FilePen, web: Globe, generic: Wrench }

function text(key: string): string | undefined {
  const value = input.value[key]
  return typeof value === "string" && value.trim() !== "" ? value : undefined
}

const name = computed(() => props.request.call.name.trim().toLowerCase())
const edit = computed(() => name.value === "edit" ? diffLines(input.value, undefined) : [])
const content = computed(() => name.value === "write" ? text("content") : undefined)
/** Input fields other than the ones a panel already shows. */
const extra = computed(() => Object.entries(input.value)
  .filter(([key]) => !["command", "cwd", "path", "pattern", "oldText", "newText", "content", "query", "url"].includes(key))
  .map(([key, value]) => ({ key, value: typeof value === "string" ? value : JSON.stringify(value) })))

async function respond(decision: ApprovalDecision): Promise<void> {
  if (pending.value) return
  pending.value = true
  error.value = ""
  try {
    await chat.respond(props.sessionId, props.request, decision)
  } catch (cause) {
    error.value = cause instanceof Error && cause.message ? cause.message : t("prompt.permissionSubmitFailed")
    pending.value = false
  }
}

function onKeydown(event: KeyboardEvent): void {
  if (pending.value || event.defaultPrevented || document.querySelector("dialog[open]") !== null) return
  const target = event.target
  if (target instanceof HTMLElement && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable)) return
  if (event.key === "Enter" || event.key === "y" || event.key === "Y") {
    event.preventDefault()
    void respond("allow")
  } else if (event.key === "Escape" || event.key === "n" || event.key === "N") {
    event.preventDefault()
    void respond("deny")
  }
}

onMounted(() => window.addEventListener("keydown", onKeydown))
onBeforeUnmount(() => window.removeEventListener("keydown", onKeydown))
</script>

<template>
  <article class="prompt-card" data-inline-interaction-prompt>
    <header class="prompt-header">
      <span class="prompt-icon" aria-hidden="true"><component :is="ICONS[display.renderer]" :size="12" :stroke-width="2.25" /></span>
      <h2 class="prompt-title">{{ t("prompt.actionRequest", { action: display.renderer === "generic" ? request.call.name : t(display.labelKey) }) }}</h2>
      <span class="ui-badge" data-tone="warning">{{ t("prompt.awaitingApproval") }}</span>
    </header>
    <p v-if="request.reason" class="prompt-subtitle">{{ request.reason }}</p>

    <div v-if="display.renderer === 'command'" class="approval-command">
      <div><span class="approval-muted">$</span> {{ text("command") }}</div>
      <div v-if="text('cwd')" class="approval-muted">  cwd: {{ text("cwd") }}</div>
    </div>
    <div v-else-if="display.renderer === 'file'" class="approval-panel">
      <div class="approval-panel-header">{{ text("path") ?? request.call.name }}</div>
      <div v-if="edit.length" class="approval-panel-body">
        <div v-for="(line, index) in edit" :key="index" :class="line.sign === '-' ? 'approval-diff-del' : 'approval-diff-add'">{{ line.sign }} {{ line.text }}</div>
      </div>
      <div v-else-if="content !== undefined" class="approval-panel-body">
        <div v-for="(line, index) in content.split('\n')" :key="index" class="approval-code-line"><span class="approval-line-number">{{ index + 1 }}</span>{{ line }}</div>
      </div>
    </div>
    <div v-else-if="display.renderer === 'search' || display.renderer === 'web'" class="approval-panel">
      <div class="approval-panel-header">{{ t("prompt.input") }}</div>
      <div class="approval-panel-body">
        <div v-if="text('pattern')">pattern: {{ text("pattern") }}</div>
        <div v-if="text('query')">query: {{ text("query") }}</div>
        <div v-if="text('url')">url: {{ text("url") }}</div>
        <div v-if="text('path')" class="approval-muted">path: {{ text("path") }}</div>
      </div>
    </div>
    <div v-else class="approval-panel">
      <div class="approval-panel-header is-mono">{{ request.call.name }}</div>
      <pre class="approval-panel-body">{{ JSON.stringify(input, null, 2) }}</pre>
    </div>
    <div v-if="display.renderer !== 'generic' && extra.length" class="approval-panel">
      <div class="approval-panel-body approval-muted"><div v-for="line in extra" :key="line.key">{{ line.key }}: {{ line.value }}</div></div>
    </div>

    <p v-if="error" class="prompt-error" role="alert">{{ error }}</p>
    <div class="prompt-buttons">
      <button type="button" class="ui-button" data-size="md" data-variant="ghost" :disabled="pending" @click="respond('deny')">{{ t("prompt.reject") }}</button>
      <button type="button" class="ui-button" data-size="md" data-variant="primary" :disabled="pending" @click="respond('allow')">{{ t("prompt.allowOnce") }}</button>
    </div>
  </article>
</template>
