<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch, type Component } from "vue"
import { useI18n } from "vue-i18n"
import { FilePen, Globe, Search, Terminal, Wrench } from "lucide-vue-next"
import type { ApprovalDecision, ApprovalRequest, MemberApprovalDescriptor } from "../../rpc/protocol"
import { RpcError } from "../../rpc/client"
import { useChatStore } from "../../stores/chat"
import { alwaysCommand, fileGrant } from "../../approvals"
import { approvalKeyDecision } from "../lines/line-state"
import { diffLines, resolveToolDisplay, type ToolRenderer } from "./tool-display"

/**
 * The tool approval prompt: it takes the composer's place, names the action,
 * shows the call's input, and answers Deny or Allow (Enter/y, Escape/n). A
 * simple command also offers "Always allow" and a file tool offers "Allow for
 * this session"; each saves its rule or grant before it allows.
 */
const props = defineProps<{ sessionId: string; request: ApprovalRequest; keyboard?: boolean; rulesAvailable?: boolean }>()
const { t } = useI18n()
const chat = useChatStore()
const pending = ref(false)
const error = ref("")

const isMember = computed(() => props.request.call.name === "member_approval_allow")
const sessionGrant = computed(() => !isMember.value && fileGrant(props.request) !== undefined)
const alwaysAllowed = computed(() => !isMember.value && props.rulesAvailable === true && alwaysCommand(props.request) !== undefined)
const member = ref<MemberApprovalDescriptor>()
const loading = ref(false)
const retryable = ref(true)
async function loadMember(): Promise<boolean> {
  loading.value = true
  error.value = ""
  try {
    const detail = await chat.memberApproval(props.sessionId, props.request)
    if (detail.state === "ended") { await chat.respond(props.sessionId, props.request, "deny"); return false }
    member.value = detail.descriptor
    return true
  } catch (cause) {
    member.value = undefined
    retryable.value = !(cause instanceof RpcError && cause.code === -32601)
    error.value = cause instanceof Error ? cause.message : "Cannot review member action"
    return false
  } finally { loading.value = false }
}
const action = computed(() => member.value?.call ?? props.request.call)
const input = computed(() => action.value.input)
const display = computed(() => resolveToolDisplay(action.value.name, input.value))
const ICONS: Record<ToolRenderer, Component> = { command: Terminal, search: Search, file: FilePen, web: Globe, generic: Wrench }

function text(key: string): string | undefined {
  const value = input.value[key]
  return typeof value === "string" && value.trim() !== "" ? value : undefined
}

const name = computed(() => action.value.name.trim().toLowerCase())
const edit = computed(() => name.value === "edit" ? diffLines(input.value, undefined) : [])
const content = computed(() => name.value === "write" ? text("content") : undefined)
/** Input fields other than the ones a panel already shows. */
const extra = computed(() => Object.entries(input.value)
  .filter(([key]) => !["command", "cwd", "path", "pattern", "oldText", "newText", "content", "query", "url"].includes(key))
  .map(([key, value]) => ({ key, value: typeof value === "string" ? value : JSON.stringify(value) })))

type Choice = ApprovalDecision | "always" | "session"

async function respond(choice: Choice): Promise<void> {
  if (pending.value || (choice === "allow" && isMember.value && (loading.value || member.value === undefined))) return
  pending.value = true
  error.value = ""
  try {
    if (choice === "allow" && isMember.value && !await loadMember()) { pending.value = false; return }
    if (choice === "always") await chat.allowAlways(props.sessionId, props.request)
    else if (choice === "session") await chat.allowForSession(props.sessionId, props.request)
    else await chat.respond(props.sessionId, props.request, choice)
  } catch (cause) {
    error.value = cause instanceof Error && cause.message ? cause.message : t("prompt.permissionSubmitFailed")
    pending.value = false
  }
}

function onKeydown(event: KeyboardEvent): void {
  const decision = approvalKeyDecision(event, {
    keyboard: props.keyboard,
    pending: pending.value,
    dialogOpen: document.querySelector("dialog[open]") !== null,
  })
  if (decision === undefined) return
  event.preventDefault()
  void respond(decision)
}

let listening = false
function attach(): void {
  if (listening || props.keyboard === false) return
  window.addEventListener("keydown", onKeydown)
  listening = true
}

function detach(): void {
  if (!listening) return
  window.removeEventListener("keydown", onKeydown)
  listening = false
}

onMounted(() => { attach(); if (isMember.value) void loadMember() })
onBeforeUnmount(detach)
watch(() => props.keyboard, () => props.keyboard === false ? detach() : attach())
</script>

<template>
  <article class="prompt-card" data-inline-interaction-prompt>
    <header class="prompt-header">
      <span class="prompt-icon" aria-hidden="true"><component :is="ICONS[display.renderer]" :size="12" :stroke-width="2.25" /></span>
      <h2 class="prompt-title">{{ t("prompt.actionRequest", { action: display.renderer === "generic" ? request.call.name : t(display.labelKey) }) }}</h2>
      <span class="ui-badge" data-tone="warning">{{ t("prompt.awaitingApproval") }}</span>
    </header>
    <p v-if="request.reason" class="prompt-subtitle">{{ request.reason }}</p>

    <div v-if="isMember" class="approval-panel">
      <div class="approval-panel-header">Subagent tool approval · Leader branch: {{ request.branchId }}</div>
      <template v-if="member">
        <p>Member: {{ member.member.agentId }} · Session: {{ member.member.sessionId }} · Branch: {{ member.member.branchId }}</p>
        <p>{{ member.call.name }}: {{ member.reason }}</p>
        <pre class="approval-panel-body">{{ JSON.stringify(member.call.input, null, 2) }}</pre>
      </template>
      <p v-else>{{ loading ? "Loading member action..." : "Cannot review member action" }}</p>
    </div>
    <div v-else-if="display.renderer === 'command'" class="approval-command">
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
    <div v-if="!isMember && display.renderer !== 'generic' && extra.length" class="approval-panel">
      <div class="approval-panel-body approval-muted"><div v-for="line in extra" :key="line.key">{{ line.key }}: {{ line.value }}</div></div>
    </div>

    <p v-if="error" class="prompt-error" role="alert">{{ error }}</p>
    <div class="prompt-buttons">
      <button type="button" class="ui-button" data-size="md" data-variant="ghost" :disabled="pending" @click="respond('deny')">{{ t("prompt.reject") }}</button>
      <button v-if="isMember && error && retryable" type="button" class="ui-button" :disabled="loading" @click="loadMember()">Retry</button>
      <button v-if="sessionGrant" type="button" class="ui-button" data-size="md" data-variant="secondary" :disabled="pending" @click="respond('session')">{{ t("prompt.allowSession") }}</button>
      <button v-if="alwaysAllowed" type="button" class="ui-button" data-size="md" data-variant="secondary" :disabled="pending" @click="respond('always')">{{ t("prompt.allowAlways") }}</button>
      <button type="button" class="ui-button" data-size="md" data-variant="primary" :disabled="pending || (isMember && (loading || !member))" @click="respond('allow')">{{ t("prompt.allowOnce") }}</button>
    </div>
  </article>
</template>
