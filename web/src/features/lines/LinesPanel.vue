<script setup lang="ts">
import { computed } from "vue"
import { useI18n } from "vue-i18n"
import { MAIN_BRANCH } from "../../rpc/protocol"
import { useChatStore } from "../../stores/chat"
import { useToastStore } from "../../stores/toast"
import { useUiStore } from "../../stores/ui"
import LineConversation from "./LineConversation.vue"
import LinesTree from "./LinesTree.vue"
import { lineName, lineStateTone, shortId } from "./line-state"

const props = defineProps<{ sessionId: string }>()
const { t } = useI18n()
const chat = useChatStore()
const ui = useUiStore()
const toast = useToastStore()

const view = computed(() => chat.views[props.sessionId])
const tabs = computed(() => ui.lineTabs[props.sessionId] ?? [])
const active = computed(() => ui.activeLineTab[props.sessionId])
const lineCount = computed(() => view.value?.lines.filter((line) => line.id !== MAIN_BRANCH).length ?? 0)

function line(branchId: string) {
  return view.value?.lines.find((candidate) => candidate.id === branchId)
}

function title(branchId: string): string {
  return lineName(t, line(branchId), branchId)
}

function tone(branchId: string): "primary" | "warning" | "danger" | undefined {
  if (view.value?.approvals.some((request) => request.branchId === branchId) === true) return "warning"
  const found = line(branchId)
  return found === undefined ? undefined : lineStateTone(found.state)
}

function busy(branchId: string): boolean {
  const branch = view.value?.branches[branchId]
  return branch?.running === true || view.value?.approvals.some((request) => request.branchId === branchId) === true
}

function showDot(branchId: string): boolean {
  const state = line(branchId)?.state.kind
  return busy(branchId) || state === "ending" || state === "ended" || state === "ended-itself" || state === "ended-by-operation" || state === "ended-with" || state === "creation-failed"
}

function openLines(): void {
  ui.openLines(props.sessionId)
}

function activate(branchId: string): void {
  chat.ensureLine(props.sessionId, branchId)
  ui.activateLine(props.sessionId, branchId)
}

function closeTab(branchId: string): void {
  ui.closeLineTab(props.sessionId, branchId)
}

function newLine(): void {
  const at = view.value?.lastTurnEnd
  if (at === undefined) {
    toast.push({ tone: "info", title: t("lines.noFinishedTurn") })
    return
  }
  ui.requestNewLine({ sessionId: props.sessionId, parentBranchId: MAIN_BRANCH, at, text: "" })
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return
  const order = [undefined, ...tabs.value]
  const index = order.findIndex((id) => id === active.value)
  const next = order[(index + (event.key === "ArrowRight" ? 1 : -1) + order.length) % order.length]
  event.preventDefault()
  if (next === undefined) openLines()
  else activate(next)
}
</script>

<template>
  <div class="lines-panel" role="region" :aria-label="t('lines.panelAria')">
    <header class="lines-tabs">
      <div class="lines-tablist" role="tablist" :aria-label="t('lines.title')" @keydown="onKeydown">
        <button
          type="button"
          class="lines-tab"
          role="tab"
          :aria-selected="active === undefined"
          :class="{ 'is-active': active === undefined }"
          @click="openLines"
        >
          <UIcon name="i-lucide-list-tree" class="size-3.5 shrink-0" aria-hidden="true" />
          <span class="lines-tab-title">{{ t("lines.title") }}</span>
          <UBadge v-if="lineCount > 0" color="neutral" variant="subtle" size="sm">{{ lineCount }}</UBadge>
        </button>
        <div v-for="branchId in tabs" :key="branchId" class="lines-tab-wrap" :class="{ 'is-active': active === branchId }">
          <button type="button" class="lines-tab" role="tab" :aria-selected="active === branchId" @click="activate(branchId)">
            <UIcon name="i-lucide-git-branch" class="size-3.5 shrink-0" :class="{ 'is-running': busy(branchId) }" aria-hidden="true" />
            <span class="lines-tab-title">{{ title(branchId) }}</span>
            <span class="lines-tab-id">{{ shortId(branchId) }}</span>
            <span v-if="showDot(branchId)" class="lines-tab-dot" :data-tone="tone(branchId)" aria-hidden="true" />
          </button>
          <button type="button" class="lines-tab-close" :aria-label="t('lines.closeTab')" @click.stop="closeTab(branchId)">
            <UIcon name="i-lucide-x" class="size-3" aria-hidden="true" />
          </button>
        </div>
      </div>
      <div class="lines-tab-actions">
        <UTooltip :text="t('lines.newLine')">
          <UButton color="neutral" variant="ghost" size="sm" square icon="i-lucide-plus" :aria-label="t('lines.newLine')" @click="newLine" />
        </UTooltip>
        <UTooltip :text="ui.detailExpanded ? t('lines.collapse') : t('lines.expand')">
          <UButton color="neutral" variant="ghost" size="sm" square :icon="ui.detailExpanded ? 'i-lucide-minimize-2' : 'i-lucide-maximize-2'" :aria-label="ui.detailExpanded ? t('lines.collapse') : t('lines.expand')" @click="ui.detailExpanded = !ui.detailExpanded" />
        </UTooltip>
        <UTooltip :text="t('lines.closePanel')">
          <UButton color="neutral" variant="ghost" size="sm" square icon="i-lucide-x" :aria-label="t('lines.closePanel')" @click="ui.closeDetail" />
        </UTooltip>
      </div>
    </header>

    <div class="lines-body">
      <div v-if="!view || view.status === 'loading'" class="lines-loading" role="status">{{ t("common.loading") }}</div>
      <LinesTree v-else-if="active === undefined" :session-id="sessionId" />
      <LineConversation v-else :key="active" :session-id="sessionId" :branch-id="active" />
    </div>
  </div>
</template>

<style scoped>
.lines-panel { display: flex; min-width: 0; min-height: 0; flex-direction: column; background: var(--background); }
.lines-tabs { display: flex; min-width: 0; height: 48px; flex: none; align-items: center; gap: var(--space-1); padding: 0 var(--space-2) 0 var(--space-3); border-bottom: 1px solid var(--border); }
.lines-tablist { display: flex; min-width: 0; flex: 1; align-items: center; gap: 2px; overflow-x: auto; scrollbar-width: none; }
.lines-tablist::-webkit-scrollbar { display: none; }
.lines-tab-wrap { display: flex; min-width: 0; align-items: center; border-radius: var(--control-sm-radius); }
.lines-tab-wrap.is-active, .lines-tab.is-active { background: var(--hover); }
.lines-tab { display: flex; min-width: 0; height: 28px; align-items: center; gap: 6px; padding: 0 var(--space-1) 0 var(--space-2); border: 0; border-radius: var(--control-sm-radius); background: transparent; color: var(--muted-foreground); font: inherit; font-size: var(--text-sm); white-space: nowrap; cursor: pointer; }
.lines-tab.is-active { color: var(--foreground); }
.lines-tab > svg { flex: none; color: var(--muted-foreground); }
.lines-tab > svg.is-running { color: var(--primary); }
.lines-tab-title { max-width: 150px; overflow: hidden; text-overflow: ellipsis; }
.lines-tab-id { color: var(--muted-foreground); font-family: var(--font-mono); font-size: var(--text-2xs); }
.lines-tab-dot { width: 6px; height: 6px; flex: none; border-radius: 50%; background: var(--warning); }
.lines-tab-dot[data-tone="primary"] { background: var(--primary); }
.lines-tab-dot[data-tone="danger"] { background: var(--danger); }
.lines-tab-close { display: grid; width: 22px; height: 22px; flex: none; place-items: center; margin-right: 3px; border: 0; border-radius: var(--control-xs-radius); background: transparent; color: var(--muted-foreground); cursor: pointer; }
.lines-tab-close:hover { background: var(--hover); color: var(--foreground); }
.lines-tab-actions { display: flex; flex: none; align-items: center; gap: 1px; }
.lines-body { display: flex; min-width: 0; min-height: 0; flex: 1; flex-direction: column; }
.lines-loading { display: flex; flex: 1; align-items: center; justify-content: center; color: var(--muted-foreground); font-size: var(--text-sm); }
</style>
