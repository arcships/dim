<script setup lang="ts">
import { computed, ref, watch } from "vue"
import { useI18n } from "vue-i18n"
import { GitBranch, LoaderCircle, RotateCcw } from "lucide-vue-next"
import { MAIN_BRANCH, type DraftAttachments } from "../../rpc/protocol"
import { useChatStore } from "../../stores/chat"
import { useConnectionStore } from "../../stores/connection"
import { useUiStore } from "../../stores/ui"
import ApprovalPrompt from "../chat/ApprovalPrompt.vue"
import ComposerPanel from "../chat/ComposerPanel.vue"
import QuestionPrompt from "../chat/QuestionPrompt.vue"
import TimelineView from "../chat/TimelineView.vue"
import { lineName, lineStateLabel, lineStateTone, shortId } from "./line-state"

const props = defineProps<{ sessionId: string; branchId: string }>()
const { t } = useI18n()
const chat = useChatStore()
const connection = useConnectionStore()
const ui = useUiStore()
const composer = ref<InstanceType<typeof ComposerPanel>>()

const view = computed(() => chat.views[props.sessionId])
const branch = computed(() => view.value?.branches[props.branchId])
const line = computed(() => view.value?.lines.find((candidate) => candidate.id === props.branchId))
const approval = computed(() => view.value?.approvals.find((request) => request.branchId === props.branchId))
const approvalKeyboard = computed(() => ui.approvalKeyboard(props.sessionId, props.branchId, view.value?.approvals ?? []))
const questions = computed(() => view.value?.questions.filter((ask) => ask.branchId === props.branchId) ?? [])
const rulesAvailable = computed(() => connection.supports("permission.rules") && connection.supports("permission.set_rules"))
const searchFiles = computed(() => view.value?.methods.includes("session.files.search") ? (query: string) => chat.searchFiles(props.sessionId, query) : undefined)
const open = computed(() => line.value === undefined
  ? branch.value?.status !== "failed"
  : line.value.state.kind === "working" || line.value.state.kind === "waiting")
const title = computed(() => lineName(t, line.value, props.branchId))
const state = computed(() => line.value?.state ?? (branch.value?.running ? { kind: "working" as const } : { kind: "waiting" as const }))
const parentName = computed(() => {
  const parentId = line.value?.parentId
  if (parentId === null || parentId === undefined) return ""
  if (parentId === MAIN_BRANCH) return t("lines.main")
  const parent = view.value?.lines.find((candidate) => candidate.id === parentId)
  return parent === undefined ? shortId(parentId) : lineName(t, parent, parentId)
})
const cut = computed(() => line.value?.cut === null || line.value?.cut === undefined
  ? ""
  : t("lines.afterMessage", { count: line.value.cut.messages }))
const rows = computed(() => branch.value?.rows ?? [])
const running = computed(() => branch.value?.running ?? false)
const stopping = computed(() => branch.value?.stopping ?? false)

watch(() => [props.sessionId, props.branchId], () => chat.ensureLine(props.sessionId, props.branchId), { immediate: true })

function send(text: string, attachments: DraftAttachments): Promise<void> {
  return chat.submit(props.sessionId, props.branchId, text, attachments)
}

function quote(text: string): void {
  void composer.value?.quote(text)
}

function stop(): Promise<void> {
  return chat.stop(props.sessionId, props.branchId)
}

function branchHere(text: string, at: number): void {
  ui.requestNewLine({ sessionId: props.sessionId, parentBranchId: props.branchId, at, text })
}

function openLine(branchId: string): void {
  chat.ensureLine(props.sessionId, branchId)
  ui.openLine(props.sessionId, branchId)
}

function end(): void {
  ui.requestEndLine({ sessionId: props.sessionId, branchId: props.branchId, purpose: title.value })
}

function newChild(): void {
  const at = branch.value?.lastTurnEnd
  if (at === undefined) return
  ui.requestNewLine({ sessionId: props.sessionId, parentBranchId: props.branchId, at, text: "" })
}

async function createBranch(prompt: string): Promise<void> {
  const at = branch.value?.lastTurnEnd
  if (at === undefined) throw new Error(t("lines.noFinishedTurn"))
  const branchId = await chat.createLine(props.sessionId, props.branchId, at, prompt)
  ui.openLine(props.sessionId, branchId)
}

function retry(): void {
  chat.retryLine(props.sessionId, props.branchId)
}

function noModel(): void {
}
</script>

<template>
  <section class="line-conversation" :aria-label="t('lines.lineAria', { name: title })">
    <header class="line-crumb">
      <div class="line-crumb-main">
        <GitBranch :size="14" aria-hidden="true" />
        <strong>{{ title }}</strong>
        <code>{{ shortId(branchId) }}</code>
        <span class="line-state" :data-tone="lineStateTone(state)">{{ lineStateLabel(t, state) }}</span>
      </div>
      <div class="line-crumb-meta">
        <span v-if="parentName">{{ t("lines.from", { name: parentName }) }}<template v-if="cut"> · {{ cut }}</template></span>
        <span v-else>{{ t("lines.main") }}</span>
        <button v-if="open && branch?.lastTurnEnd !== undefined" type="button" class="ui-button" data-variant="ghost" data-size="xs" @click="newChild">{{ t("lines.newChild") }}</button>
        <button v-if="open && line?.id !== 'main'" type="button" class="ui-button" data-variant="ghost" data-size="xs" @click="end">{{ t("lines.end") }}</button>
      </div>
    </header>

    <div v-if="!view || !branch || branch.status === 'loading'" class="line-loading" role="status">
      <LoaderCircle :size="16" class="is-spinning" aria-hidden="true" />
      <span>{{ t("common.loading") }}</span>
    </div>
    <div v-else-if="branch.status === 'failed'" class="line-loading line-error" role="alert">
      <p>{{ branch.error || t("lines.loadFailed") }}</p>
      <button type="button" class="ui-button" data-variant="secondary" data-size="sm" @click="retry">
        <RotateCcw aria-hidden="true" />{{ t("lines.retry") }}
      </button>
    </div>
    <template v-else>
      <TimelineView
        :rows="rows"
        :running="running"
        :quotable="open && !approval && questions.length === 0"
        :branchable="open && !approval && questions.length === 0"
        :empty="t('lines.empty')"
        variant="panel"
        @branch="branchHere"
        @open-line="openLine"
        @quote="quote"
      />
      <div v-if="approval" class="line-approval">
        <ApprovalPrompt :key="JSON.stringify([approval.branchId, approval.requestId])" :session-id="sessionId" :request="approval" :keyboard="approvalKeyboard" :rules-available="rulesAvailable" />
      </div>
      <div v-else-if="questions.length" class="line-approval question-prompts">
        <QuestionPrompt v-for="question in questions" :key="JSON.stringify([question.branchId, question.requestId, question.callId])" :session-id="sessionId" :question="question" />
      </div>
      <ComposerPanel
        v-else-if="open"
        ref="composer"
        :running="running"
        :stopping="stopping"
        :model="view.model"
        :send="send"
        :stores-pastes="view?.methods.includes('attachment.put') === true"
        :search-files="searchFiles"
        :stop="stop"
        :model-selectable="false"
        :select-model="noModel"
        :placeholder="t('lines.placeholder')"
        :create-branch="connection.supports('session.branch_create') ? createBranch : undefined"
        :open-lines="() => ui.openLines(sessionId)"
        variant="panel"
      >
        <template #status>
          <span class="line-status">⑂ {{ lineStateLabel(t, state) }}</span>
        </template>
      </ComposerPanel>
      <div v-else class="line-readonly">
        <span>⑂ {{ lineStateLabel(t, state) }}</span>
        <span>{{ t("lines.readOnly") }}</span>
      </div>
    </template>
  </section>
</template>

<style scoped>
.line-conversation { display: flex; min-width: 0; min-height: 0; flex: 1; flex-direction: column; background: var(--background); }
.line-conversation > :deep(.timeline-shell) { min-height: 0; flex: 1 1 auto; }
.line-crumb { display: flex; min-width: 0; flex: none; flex-direction: column; gap: var(--space-1); padding: var(--space-2) var(--space-3); border-bottom: 1px solid var(--border); }
.line-crumb-main, .line-crumb-meta { display: flex; min-width: 0; align-items: center; gap: var(--space-2); }
.line-crumb-meta { flex-wrap: wrap; }
.line-crumb-main > svg { flex: none; color: var(--primary); }
.line-crumb-main strong { min-width: 0; overflow: hidden; color: var(--foreground); font-size: var(--text-md); font-weight: var(--weight-medium); text-overflow: ellipsis; white-space: nowrap; }
.line-crumb-main code { flex: none; color: var(--muted-foreground); font-family: var(--font-mono); font-size: var(--text-xs); }
.line-crumb-meta { color: var(--muted-foreground); font-size: var(--text-xs); }
.line-crumb-meta > span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.line-crumb-meta > button { margin-left: auto; flex: none; }
.line-state[data-tone="primary"], .line-status { color: var(--primary); }
.line-state[data-tone="warning"] { color: var(--warning); }
.line-state[data-tone="danger"] { color: var(--danger); }
.line-state { flex: none; }
.line-loading { display: flex; min-height: 0; flex: 1; align-items: center; justify-content: center; gap: var(--space-2); color: var(--muted-foreground); font-size: var(--text-sm); }
.line-error { flex-direction: column; padding: var(--space-4); text-align: center; }
.line-error p { margin: 0; color: var(--danger); }
.line-approval { flex: none; padding: var(--space-2) var(--space-3) var(--space-3); }
.line-readonly { display: flex; flex: none; flex-direction: column; gap: var(--space-1); padding: var(--space-3); border-top: 1px solid var(--border); color: var(--muted-foreground); font-size: var(--text-sm); }
.line-status { color: var(--muted-foreground); font-size: var(--text-xs); }
</style>
