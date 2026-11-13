<script setup lang="ts">
import { computed, ref, watch } from "vue"
import { RouterLink, useRouter } from "vue-router"
import { useI18n } from "vue-i18n"
import BrandMark from "../../components/BrandMark.vue"
import ConfirmDialog from "../../components/ConfirmDialog.vue"
import ApprovalPrompt from "./ApprovalPrompt.vue"
import ComposerPanel from "./ComposerPanel.vue"
import QueuedInputs from "./QueuedInputs.vue"
import QuestionPrompt from "./QuestionPrompt.vue"
import TimelineView from "./TimelineView.vue"
import ExtensionMenu from "../extensions/ExtensionMenu.vue"
import { MAIN_BRANCH, type ContextUsage, type DraftAttachments, type ModelId, type PermissionPreset } from "../../rpc/protocol"
import { goalCall, goalObjective, parseGoalRequest, type Goal, type GoalStatus } from "../../goal"
import { useChatStore } from "../../stores/chat"
import { useConnectionStore } from "../../stores/connection"
import { useModelsStore } from "../../stores/models"
import { useSessionsStore, workspaceName } from "../../stores/sessions"
import { useToastStore } from "../../stores/toast"
import { useUiStore } from "../../stores/ui"

const props = defineProps<{ sessionId: string }>()
const { t } = useI18n()
const models = useModelsStore()
const chat = useChatStore()
const connection = useConnectionStore()
const sessions = useSessionsStore()
const ui = useUiStore()
const toast = useToastStore()
const router = useRouter()
const forkConfirm = ref<InstanceType<typeof ConfirmDialog>>()

watch(() => props.sessionId, (id) => chat.open(id), { immediate: true })

const view = computed(() => chat.views[props.sessionId])
const summary = computed(() => sessions.byId.get(props.sessionId))
const title = computed(() => summary.value?.title ?? summary.value?.preview ?? t("sidebar.untitled"))
const parentId = computed(() => summary.value?.parentSessionId)
const parent = computed(() => parentId.value === undefined ? undefined : sessions.byId.get(parentId.value))
const children = computed(() => sessions.summaries.filter((session) => session.parentSessionId === props.sessionId))
const approval = computed(() => view.value?.approvals.find((request) => request.branchId === MAIN_BRANCH))
const questions = computed(() => view.value?.questions.filter((ask) => ask.branchId === MAIN_BRANCH) ?? [])
const rulesAvailable = computed(() => connection.supports("permission.rules") && connection.supports("permission.set_rules"))
const storesPastes = computed(() => view.value?.methods.includes("attachment.put") === true)
const searchFiles = computed(() => view.value?.methods.includes("session.files.search") ? (query: string) => chat.searchFiles(props.sessionId, query) : undefined)
const permissionControl = computed(() => {
  const session = view.value
  if (session?.status !== "ready" || !connection.supports("session.permission.get") || !connection.supports("session.permission.update")) return undefined
  return { policy: session.permission, select: setPermission }
})
const approvalKeyboard = computed(() => ui.approvalKeyboard(props.sessionId, MAIN_BRANCH, view.value?.approvals ?? []))
const composer = ref<InstanceType<typeof ComposerPanel>>()
const lineCount = computed(() => view.value?.lines.filter((line) => line.id !== MAIN_BRANCH).length ?? 0)
const linesBusy = computed(() => {
  const approvals = view.value?.approvals ?? []
  return view.value?.lines.some((line) => line.id !== MAIN_BRANCH && (line.state.kind === "working" || approvals.some((request) => request.branchId === line.id))) ?? false
})
const branchable = computed(() => connection.supports("session.branch_end") && view.value?.status === "ready")
const canNewLine = computed(() => connection.supports("session.branch_create") && view.value?.status === "ready")
/** The reasoning effort values the main line's model accepts; undefined until the models are read or when it has none. */
const effortChoices = computed(() => {
  const model = view.value?.model
  if (model === undefined) return undefined
  const entry = models.providers?.find((provider) => provider.providerId === model.providerId)?.models.find((candidate) => candidate.id === model.modelId)
  return entry?.reasoningEfforts
})
watch(() => view.value?.status, (status) => { if (status === "ready") void models.load() }, { immediate: true })
const sessionCommands = computed(() => {
  const session = view.value
  if (session?.status !== "ready") return undefined
  return {
    compact: session.methods.includes("session.context.compact") ? compact : undefined,
    usage: session.methods.includes("session.context.usage") ? usage : undefined,
    fork: connection.supports("session.fork") && session.lastTurnEnd !== undefined ? fork : undefined,
    goal: session.methods.includes("session.goal.set") ? goal : undefined,
    effort: effortChoices.value !== undefined && effortChoices.value.length > 0 ? effort : undefined,
  }
})

function send(text: string, attachments: DraftAttachments): Promise<void> {
  return chat.submit(props.sessionId, MAIN_BRANCH, text, attachments)
}

function stop(): Promise<void> {
  return chat.stop(props.sessionId, MAIN_BRANCH)
}

function selectModel(model: ModelId): Promise<void> {
  return chat.setModel(props.sessionId, model)
}

function openLine(branchId: string): void {
  chat.ensureLine(props.sessionId, branchId)
  ui.openLine(props.sessionId, branchId)
}

function branch(text: string, cut: number): void {
  ui.requestNewLine({ sessionId: props.sessionId, parentBranchId: MAIN_BRANCH, at: cut, text })
}

async function createBranch(prompt: string): Promise<void> {
  const at = view.value?.lastTurnEnd
  if (at === undefined) throw new Error(t("lines.noFinishedTurn"))
  const branchId = await chat.createLine(props.sessionId, MAIN_BRANCH, at, prompt)
  ui.openLine(props.sessionId, branchId)
}

function failure(cause: unknown): void {
  toast.push({ tone: "danger", title: t("command.failed"), description: cause instanceof Error ? cause.message : String(cause) })
}

async function compact(): Promise<void> {
  try {
    const result = await chat.compact(props.sessionId)
    if (result.status === "completed") toast.push({ tone: "success", title: t("command.compacted") })
    else toast.push({ tone: "info", title: t("command.compactSkipped"), description: result.reason })
  } catch (cause) {
    failure(cause)
  }
}

/** `/usage`'s one line, as the TUI words it. */
function usageLine(reading: ContextUsage): string {
  const input = Math.round(reading.estimatedInputTokens)
  const window = Math.round(reading.contextWindow)
  return t("command.usage", {
    input: input.toLocaleString(),
    window: window.toLocaleString(),
    percent: window > 0 ? Math.floor((input * 100) / window) : 0,
    basis: reading.baselineSource === "provider_baseline" ? t("command.basisProvider") : t("command.basisEstimated"),
    output: Math.round(reading.outputTokens).toLocaleString(),
    threshold: Math.round(reading.thresholdTokens).toLocaleString(),
  })
}

async function usage(): Promise<void> {
  try {
    toast.push({ tone: "info", title: t("command.usageTitle"), description: usageLine(await chat.usage(props.sessionId)) })
  } catch (cause) {
    failure(cause)
  }
}

function statusWords(status: GoalStatus): string {
  switch (status.kind) {
    case "active": return t("goal.status.active")
    case "paused": return t("goal.status.paused", { reason: status.reason.replaceAll("_", " ") })
    case "completed": return t("goal.status.completed")
    case "blocked": return t("goal.status.blocked")
    case "none": return t("goal.status.none")
  }
}

/** `/goal` with no words: the goal in force on main. */
function showGoal(current: Goal | undefined): void {
  if (current === undefined) {
    toast.push({ tone: "info", title: t("goal.none") })
    return
  }
  toast.push({
    tone: "info",
    title: `${t("goal.line", { status: statusWords(current.status) })}${current.evaluator ? ` · ${t("goal.evaluator")}` : ""}`,
    description: current.objective,
  })
}

async function goal(words: string): Promise<void> {
  const request = parseGoalRequest(words)
  if (request === undefined) {
    toast.push({ tone: "info", title: t("goal.usage") })
    return
  }
  const current = view.value?.goal
  const call = goalCall(request, current)
  if (call === undefined) {
    showGoal(current)
    return
  }
  try {
    await chat.writeGoal(props.sessionId, call, goalObjective(request))
    toast.push({ tone: "success", title: t("goal.sent", { action: t(`goal.action.${call}`) }) })
  } catch (cause) {
    failure(cause)
  }
}

/** `/effort` with no words lists the choices; `/effort VALUE` sets the main line's reasoning effort. */
async function effort(words: string): Promise<void> {
  const choices = effortChoices.value
  const value = words.trim()
  if (choices === undefined) {
    toast.push({ tone: "info", title: t("command.effortUnavailable") })
    return
  }
  if (value === "") {
    toast.push({ tone: "info", title: t("command.effortChoices", { choices: choices.join(", ") }) })
    return
  }
  if (/\s/.test(value)) {
    toast.push({ tone: "info", title: t("command.effortUsage") })
    return
  }
  if (!choices.includes(value)) {
    toast.push({ tone: "danger", title: t("command.effortUnknown") })
    return
  }
  try {
    const outcome = await chat.setEffort(props.sessionId, value)
    toast.push({ tone: "success", title: t("command.effortSet", { outcome, value }) })
  } catch (cause) {
    failure(cause)
  }
}

async function setPermission(preset: PermissionPreset): Promise<void> {
  try {
    const outcome = await chat.setPermission(props.sessionId, preset)
    if (outcome === "conflict") toast.push({ tone: "info", title: t("permission.conflict") })
  } catch (cause) {
    failure(cause)
  }
}

async function fork(): Promise<void> {
  const at = view.value?.lastTurnEnd
  if (at === undefined) return
  if (!await forkConfirm.value?.open(t("fork.confirm"), { confirmLabel: t("fork.confirmLabel") })) return
  try {
    const forked = await chat.fork(props.sessionId, at)
    void sessions.refresh()
    await router.push({ name: "chat-session", params: { sessionId: forked } })
  } catch (cause) {
    failure(cause)
  }
}
</script>

<template>
  <section v-if="view?.status === 'closed'" class="removed-state route-page">
    <h1 data-route-heading tabindex="-1">{{ t("chat.removedTitle") }}</h1>
    <p>{{ t("chat.removedDescription") }}</p>
    <UButton to="/chat/new" color="neutral" variant="soft">{{ t("sidebar.newSession") }}</UButton>
  </section>

  <section v-else-if="view?.status === 'failed'" class="removed-state route-page">
    <h1 data-route-heading tabindex="-1">{{ title }}</h1>
    <p role="alert">{{ view.error }}</p>
  </section>

  <section v-else-if="!view || view.status === 'loading'" class="chat-loading" aria-busy="true">
    <h1 class="sr-only">{{ t("route.session") }}</h1>
    <BrandMark animated class="boot-mark" />
    <p>{{ t("common.loading") }}</p>
  </section>

  <section v-else class="chat-page">
    <ConfirmDialog ref="forkConfirm" />
    <div class="chat-page-top">
      <header class="session-header">
        <UButton
          v-if="ui.shellMode === 'compact'"
          class="session-nav"
          color="neutral"
          variant="ghost"
          size="md"
          square
          icon="i-lucide-menu"
          :aria-label="t('sidebar.openMenu')"
          @click="ui.drawerOpen = true"
        />
        <div class="session-heading">
          <template v-if="summary">
            <span class="workspace-crumb" :title="summary.cwd">{{ workspaceName(summary.cwd) }}</span>
            <span class="workspace-crumb-separator" aria-hidden="true">/</span>
          </template>
          <h1 data-route-heading tabindex="-1" :title="title">{{ title }}</h1>
        </div>
        <ExtensionMenu :session-id="sessionId" />
        <UButton
          v-if="connection.supports('session.branch_end')"
          class="session-lines"
          color="neutral"
          variant="ghost"
          size="sm"
          icon="i-lucide-git-branch"
          :aria-label="t('lines.open')"
          @click="ui.toggleLines(sessionId)"
        >
          <span>{{ t("lines.title") }}</span>
          <template v-if="lineCount > 0" #trailing>
            <UBadge size="sm" :color="linesBusy ? 'warning' : 'neutral'" variant="subtle" class="session-lines-badge">{{ lineCount }}</UBadge>
          </template>
        </UButton>
        <UBadge v-if="view.sessionRunning" class="session-running" color="primary" variant="subtle">
          <span class="status-dot" data-state="running" aria-hidden="true" />{{ t(view.stopping ? "status.stopping" : "status.running") }}
        </UBadge>
      </header>
      <div v-if="parentId || children.length" class="session-relations">
        <RouterLink v-if="parentId" :to="{ name: 'chat-session', params: { sessionId: parentId } }">
          {{ t("session.parent", { title: parent?.title ?? parent?.preview ?? parentId }) }}
        </RouterLink>
        <UCollapsible v-if="children.length" :key="sessionId">
          <UButton color="neutral" variant="link" size="xs" trailing-icon="i-lucide-chevron-down" class="px-0">
            {{ t("session.children", { count: children.length }) }}
          </UButton>
          <template #content>
            <ul>
              <li v-for="child in children" :key="child.sessionId">
                <RouterLink :to="{ name: 'chat-session', params: { sessionId: child.sessionId } }">
                  {{ child.title ?? child.preview ?? child.sessionId }}
                </RouterLink>
              </li>
            </ul>
          </template>
        </UCollapsible>
      </div>
    </div>
    <TimelineView
      :rows="view.rows"
      :earlier="view.earlier"
      :loading-earlier="view.loadingEarlier"
      :history-error="view.historyError"
      @earlier="chat.loadEarlier(sessionId, 'main')"
      :running="view.running"
      :retry="view.retry"
      :quotable="!approval && questions.length === 0"
      :branchable="branchable && !approval && questions.length === 0"
      @branch="branch"
      @open-line="openLine"
      @quote="composer?.quote($event)"
    />
    <div v-if="view.running && view.queued.length" class="composer-panel">
      <QueuedInputs :session-id="sessionId" :items="view.queued" />
    </div>
    <div v-if="approval" class="composer-panel">
      <ApprovalPrompt :key="JSON.stringify([approval.branchId, approval.requestId])" :session-id="sessionId" :request="approval" :keyboard="approvalKeyboard" :rules-available="rulesAvailable" />
    </div>
    <div v-else-if="questions.length" class="composer-panel question-prompts">
      <QuestionPrompt v-for="question in questions" :key="JSON.stringify([question.branchId, question.requestId, question.callId])" :session-id="sessionId" :question="question" />
    </div>
    <ComposerPanel
      v-else
      ref="composer"
      :running="view.running"
      :stopping="view.stopping"
      :model="view.model"
      :send="send"
      :stores-pastes="storesPastes"
      :search-files="searchFiles"
      :permission="permissionControl"
      :stop="stop"
      :model-selectable="connection.supports('session.model.update')"
      :select-model="selectModel"
      :create-branch="canNewLine ? createBranch : undefined"
      :open-lines="branchable ? () => ui.openLines(sessionId) : undefined"
      :session-commands="sessionCommands"
    />
  </section>
</template>

<style scoped>
.session-running { gap: 6px; }
.session-running > .status-dot { border: 0; }
.session-lines { gap: 6px; flex: none; }
.session-lines-badge { min-width: 18px; padding-inline: 4px; }
.session-relations { padding: 0 var(--space-4) var(--space-2); font-size: var(--text-sm); overflow-wrap: anywhere; }
.session-relations summary { cursor: pointer; }
.session-relations ul { max-height: 10rem; overflow-y: auto; margin: var(--space-1) 0; padding-inline-start: var(--space-6); }
.session-relations a { text-decoration: underline; }
</style>
