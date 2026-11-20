<script setup lang="ts">
import { computed, ref } from "vue"
import { useI18n } from "vue-i18n"
import { ROOT_BRANCH_ID } from "../../rpc/protocol"
import type { LineView } from "../../lines"
import { useChatStore } from "../../stores/chat"
import { useUiStore } from "../../stores/ui"
import { canContinueFrom, lineMarks, lineName, lineStateLabel, lineStateTone, shortId } from "./line-state"

const props = defineProps<{ sessionId: string }>()
const { t } = useI18n()
const chat = useChatStore()
const ui = useUiStore()

const view = computed(() => chat.views[props.sessionId])
const lines = computed(() => view.value?.lines ?? [])
const hasLines = computed(() => lines.value.some((line) => line.id !== ROOT_BRANCH_ID))
/** The line the Session's conversation is on (design §4). */
const defaultBranchId = computed(() => view.value?.defaultBranchId ?? ROOT_BRANCH_ID)
/** The refusal `session.branch_set_default` answered, shown on the row that raised it. */
const refusal = ref<{ branchId: string; message: string }>()

function open(line: LineView): void {
  if (line.state.kind === "creation-failed") return
  if (line.id === defaultBranchId.value) {
    ui.openLines(props.sessionId)
    return
  }
  chat.ensureLine(props.sessionId, line.id)
  ui.openLine(props.sessionId, line.id)
}

function marks(line: LineView): Array<"main-line" | "start"> {
  return lineMarks(line.id, defaultBranchId.value)
}

function canContinue(line: LineView): boolean {
  return canContinueFrom(line, defaultBranchId.value)
}

async function continueFrom(line: LineView): Promise<void> {
  refusal.value = undefined
  try {
    await chat.setDefaultBranch(props.sessionId, line.id)
  } catch (cause) {
    refusal.value = { branchId: line.id, message: cause instanceof Error ? cause.message : String(cause) }
  }
}

function canEnd(line: LineView): boolean {
  return line.id !== ROOT_BRANCH_ID && (line.state.kind === "working" || line.state.kind === "waiting")
}

function awaitingApproval(line: LineView): boolean {
  return view.value?.approvals.some((request) => request.branchId === line.id) === true
}

function tone(line: LineView): "primary" | "warning" | "danger" | undefined {
  return awaitingApproval(line) ? "warning" : lineStateTone(line.state)
}

function end(line: LineView): void {
  ui.requestEndLine({ sessionId: props.sessionId, branchId: line.id, purpose: lineName(t, line, line.id) })
}

function cut(line: LineView): string {
  return line.cut === null || line.cut.messages === undefined ? "" : t("lines.afterMessage", { count: line.cut.messages })
}

function parent(line: LineView): string {
  if (line.parentId === null) return ""
  const found = lines.value.find((candidate) => candidate.id === line.parentId)
  return found === undefined ? shortId(line.parentId) : lineName(t, found, line.parentId)
}
</script>

<template>
  <div class="lines-tree">
    <div v-if="!hasLines" class="lines-empty">
      <span class="lines-empty-icon" aria-hidden="true"><UIcon name="i-lucide-git-branch" class="size-7" /></span>
      <strong>{{ t("lines.emptyTitle") }}</strong>
      <p>{{ t("lines.emptyHint") }}</p>
    </div>

    <ol v-else class="lines-list" :aria-label="t('lines.title')">
      <li
        v-for="line in lines"
        :key="line.id"
        class="lines-row"
        :data-depth="line.depth"
        :data-active="ui.activeLineTab[sessionId] === line.id"
      >
        <template v-if="line.id === ROOT_BRANCH_ID">
          <button type="button" class="lines-open" :aria-label="t('lines.openLine', { name: t('lines.main') })" @click="open(line)">
            <span class="lines-state" :data-tone="tone(line)" aria-hidden="true" />
            <span class="lines-name">{{ t("lines.main") }}</span>
            <span v-for="mark in marks(line)" :key="mark" class="lines-mark">{{ t(mark === "main-line" ? "lines.mainLine" : "lines.start") }}</span>
            <span class="lines-meta">{{ t("lines.mainHint") }}</span>
          </button>
          <UButton v-if="canContinue(line)" class="lines-continue" color="neutral" variant="ghost" size="xs" :aria-label="t('lines.continueFromLine', { name: t('lines.main') })" @click="continueFrom(line)">
            {{ t("lines.continueFrom") }}
          </UButton>
        </template>
        <template v-else>
          <button type="button" class="lines-open" :aria-label="t('lines.openLine', { name: lineName(t, line, line.id) })" :disabled="line.state.kind === 'creation-failed'" @click="open(line)">
            <span class="lines-state" :data-tone="tone(line)" aria-hidden="true" />
            <span class="lines-name">{{ lineName(t, line, line.id) }}</span>
            <span class="lines-meta">
              <span v-if="line.cut !== null">{{ cut(line) }} · </span>
              <span class="lines-id">{{ shortId(line.id) }}</span>
              <span v-if="line.parentId !== ROOT_BRANCH_ID"> · {{ t("lines.from", { name: parent(line) }) }}</span>
              <span> · {{ lineStateLabel(t, line.state) }}</span>
            </span>
            <span v-for="mark in marks(line)" :key="mark" class="lines-mark">{{ t(mark === "main-line" ? "lines.mainLine" : "lines.start") }}</span>
          </button>
          <UButton v-if="canContinue(line)" class="lines-continue" color="neutral" variant="ghost" size="xs" :aria-label="t('lines.continueFromLine', { name: lineName(t, line, line.id) })" @click="continueFrom(line)">
            {{ t("lines.continueFrom") }}
          </UButton>
          <UButton v-if="canEnd(line)" class="lines-end" color="neutral" variant="ghost" size="xs" :aria-label="t('lines.endLine', { name: lineName(t, line, line.id) })" @click="end(line)">
            {{ t("lines.end") }}
          </UButton>
        </template>
        <p v-if="refusal?.branchId === line.id" class="lines-refusal" role="alert">{{ refusal.message }}</p>
      </li>
    </ol>
  </div>
</template>

<style scoped>
.lines-tree { min-height: 0; flex: 1; overflow-y: auto; padding: var(--space-2); }
.lines-list { display: flex; min-width: 0; flex-direction: column; gap: 1px; margin: 0; padding: 0; list-style: none; }
.lines-row { display: flex; min-width: 0; align-items: center; gap: var(--space-1); padding-left: var(--space-2); border-radius: var(--control-sm-radius); }
.lines-row[data-depth="1"] { padding-left: calc(var(--space-2) + 16px); }
.lines-row[data-depth="2"] { padding-left: calc(var(--space-2) + 32px); }
.lines-row[data-depth="3"] { padding-left: calc(var(--space-2) + 48px); }
.lines-row[data-depth="4"] { padding-left: calc(var(--space-2) + 64px); }
.lines-row[data-depth="5"] { padding-left: calc(var(--space-2) + 80px); }
.lines-row[data-active="true"] { background: var(--hover); }
.lines-state { width: 7px; height: 7px; flex: none; margin-inline: var(--space-1); border-radius: 50%; background: var(--muted-foreground); }
.lines-state[data-tone="primary"] { background: var(--primary); }
.lines-state[data-tone="warning"] { background: var(--warning); }
.lines-state[data-tone="danger"] { background: var(--danger); }
.lines-open { display: flex; min-width: 0; flex: 1; align-items: baseline; gap: var(--space-2); padding: var(--space-2) var(--space-1); border: 0; border-radius: var(--control-sm-radius); background: transparent; color: inherit; font: inherit; text-align: left; cursor: pointer; }
.lines-open:hover:not(:disabled) { background: var(--hover); }
.lines-open:disabled { cursor: default; opacity: .7; }
.lines-name { min-width: 0; overflow: hidden; color: var(--foreground); font-size: var(--text-md); font-weight: var(--weight-ui); text-overflow: ellipsis; white-space: nowrap; }
.lines-meta { min-width: 0; overflow: hidden; color: var(--muted-foreground); font-family: var(--font-mono); font-size: var(--text-xs); text-overflow: ellipsis; white-space: nowrap; }
.lines-id { color: var(--fg-2); }
.lines-end, .lines-continue { flex: none; opacity: 0; }
.lines-row:hover .lines-end, .lines-row:focus-within .lines-end,
.lines-row:hover .lines-continue, .lines-row:focus-within .lines-continue { opacity: 1; }
@media (hover: none) { .lines-end, .lines-continue { opacity: 1; } }
.lines-mark { flex: none; color: var(--primary); font-size: var(--text-xs); font-weight: var(--weight-medium); }
.lines-row { flex-wrap: wrap; }
.lines-refusal { width: 100%; margin: 0; padding: 0 var(--space-2) var(--space-1); color: var(--danger); font-size: var(--text-xs); overflow-wrap: anywhere; }
.lines-empty { display: flex; min-height: 100%; flex-direction: column; align-items: center; justify-content: center; gap: var(--space-2); padding: var(--space-6) var(--space-4); text-align: center; }
.lines-empty-icon { display: grid; width: 44px; height: 44px; place-items: center; border-radius: var(--radius-xl); background: var(--muted); color: var(--primary); }
.lines-empty strong { color: var(--foreground); font-size: var(--text-sm); font-weight: var(--weight-medium); }
.lines-empty p { max-width: 260px; margin: 0; color: var(--muted-foreground); font-size: var(--text-xs); line-height: var(--leading-normal); }
</style>
