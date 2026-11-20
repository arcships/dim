import { computed, ref, type Ref } from "vue"
import { useRouter } from "vue-router"
import type {
  ChatUiAnswer,
  ChatUiCapabilities,
  ChatUiCommands,
  ChatUiHost,
  ChatUiMessage,
  ChatUiTimelineEntry,
  ChatUiTimelineTurn,
} from "@dimcode/chat-ui"
import { copyText } from "../clipboard"
import { i18n } from "../i18n"
import type { TimelineRow } from "../projection"
import { ROOT_BRANCH_ID, type PendingQuestion, type QuestionAnswer } from "../rpc/protocol"
import { useChatStore } from "../stores/chat"
import { useConnectionStore } from "../stores/connection"
import { useSessionsStore } from "../stores/sessions"
import { useToastStore } from "../stores/toast"
import { createChatUiI18n } from "./i18n"
import type { WebChatRow } from "./rows"
import WebNoticeOutput from "./WebNoticeOutput.vue"

/**
 * The timeline's write actions, the App Server method each one calls, and the
 * table that announces it: hello's feature list, the Session's method table
 * (`session.methods`), or none for a built-in every connection serves.
 */
export const WEB_CHAT_ACTION_METHODS = {
  send: { method: "session.submit", served: "connection" },
  stop: { method: "session.stop", served: "connection" },
  fork: { method: "session.fork", served: "connection" },
  branch: { method: "session.branch_create", served: "connection" },
  approve: { method: "approval.respond", served: "builtin" },
  answer: { method: "session.question.respond", served: "session" },
} as const

export type WebChatAction = keyof typeof WEB_CHAT_ACTION_METHODS

/** Whether each action can run here, and, when it cannot, why. */
export type WebChatActions = Record<WebChatAction, { available: true } | { available: false; reason: string }>

export type WebChatUiHostInput = {
  sessionId: Readonly<Ref<string | undefined>>
  branchId: Readonly<Ref<string>>
  rows: Readonly<Ref<readonly WebChatRow[]>>
  running: Readonly<Ref<boolean>>
  /** The shown line takes a new child line now: it is open and no approval or question waits on it. */
  branchable: Readonly<Ref<boolean>>
  rowByKey: (key: string) => TimelineRow | undefined
  /** Display text of a notice row (`rows.ts` maps every Web-only row kind to one). */
  noticeText: (messageId: string) => string
  /** Asks the operator to confirm a fork; resolves whether they did. */
  confirmFork: (text: string) => Promise<boolean>
  /** Opens the page's new-line dialog at `cut` with `text`, the path the composer's `/branch` takes. */
  openBranch: (text: string, cut: number) => void
}

const t = (key: string, params: Record<string, string> = {}): string => i18n.global.t(key, params)

/** Rejects a command that cannot run here, saying why. */
function unavailable(key: string, params: Record<string, string> = {}): never {
  throw new Error(t(key, params))
}

/** The row turn's terminal is the renderer's detail object; the session view names only the outcome. */
function sessionTurn(turn: Extract<WebChatRow, { kind: "turn" }>["turn"]): ChatUiTimelineTurn {
  const { terminal, ...rest } = turn
  const state = terminal === undefined ? (turn.isRunning ? undefined : "completed") : terminal.status === "failed" ? "failed" : "cancelled"
  return { ...rest, ...(state === undefined ? {} : { terminal: state }) }
}

function preview(text: string): string {
  const line = text.split("\n", 1)[0]!.trim()
  return line.length > 48 ? `${line.slice(0, 47)}…` : line
}

/** One answer of the package's shape as the Web question reply: an option it names, or typed text where the question takes it. */
function questionAnswer(question: PendingQuestion, answer: ChatUiAnswer): QuestionAnswer {
  const ask = question.questions.find(candidate => candidate.id === answer.questionId)
  if (ask === undefined) return unavailable("chatTimeline.unavailable.request")
  if (ask.options.some(option => option.label === answer.answer)) return { questionId: ask.id, selected: [answer.answer] }
  if (ask.allowCustom) return { questionId: ask.id, selected: [], custom: answer.answer }
  return unavailable("chatTimeline.unavailable.answer", { answer: answer.answer })
}

/**
 * The Web host. Rows come from the caller's Projection view through `rows.ts`.
 * Each command calls the chat store action the Web already uses, after
 * checking that the connection serves its method; a command that cannot run
 * rejects with the reason. Approvals and questions stay in the
 * Web prompts above the composer, so the session view lists none, and
 * `approve`/`answer` act on the store's pending requests of the shown line.
 */
export function createWebChatUiHost(input: WebChatUiHostInput): { host: ChatUiHost; actions: Readonly<Ref<WebChatActions>> } {
  const chat = useChatStore()
  const connection = useConnectionStore()
  const sessions = useSessionsStore()
  const toast = useToastStore()
  const router = useRouter()
  const i18n = createChatUiI18n()
  const pending = ref(false)

  const actions = computed<WebChatActions>(() => {
    const id = input.sessionId.value
    const view = id === undefined ? undefined : chat.views[id]
    const entries = Object.entries(WEB_CHAT_ACTION_METHODS).map(([action, { method, served }]) => {
      if (view === undefined) return [action, { available: false, reason: t("chatTimeline.unavailable.session") }]
      if (served === "connection" && !connection.supports(method)) return [action, { available: false, reason: t("chatTimeline.unavailable.server", { method }) }]
      if (served === "session" && !view.methods.includes(method)) return [action, { available: false, reason: t("chatTimeline.unavailable.sessionMethod", { method }) }]
      if (action === "fork" && input.branchId.value !== ROOT_BRANCH_ID) return [action, { available: false, reason: t("chatTimeline.unavailable.forkLine") }]
      if (action === "branch" && !input.branchable.value) return [action, { available: false, reason: t("chatTimeline.unavailable.branchBlocked") }]
      return [action, { available: true }]
    })
    return Object.fromEntries(entries) as WebChatActions
  })
  const capabilities = computed<ChatUiCapabilities>(() => ({
    interactive: Object.values(actions.value).some(action => action.available),
    terminal: false,
    browserPanel: false,
    filePreview: false,
    mcpApps: false,
    notifications: false,
    attachments: false,
  }))

  function sessionId(action: WebChatAction): string {
    const state = actions.value[action]
    if (!state.available) throw new Error(state.reason)
    const id = input.sessionId.value
    if (id === undefined || chat.views[id] === undefined) return unavailable("chatTimeline.unavailable.session")
    return id
  }

  function userRow(messageId: string): Extract<TimelineRow, { kind: "user" }> {
    const row = input.rowByKey(messageId)
    if (row?.kind !== "user") return unavailable("chatTimeline.unavailable.message")
    return row
  }

  const commands: ChatUiCommands = {
    async send({ text, attachments, mode, branchId }) {
      const id = sessionId("send")
      if (attachments !== undefined && attachments.length > 0) unavailable("chatTimeline.unavailable.attachments")
      if (mode === "append") unavailable("chatTimeline.unavailable.mode")
      await chat.submit(id, branchId ?? input.branchId.value, text, undefined, mode)
    },
    async stop() {
      await chat.stop(sessionId("stop"), input.branchId.value)
    },
    async retry() {
      unavailable("chatTimeline.unavailable.retry")
    },
    async fork(messageId) {
      const id = sessionId("fork")
      const row = userRow(messageId)
      if (!await input.confirmFork(t("chatTimeline.forkConfirm", { preview: preview(row.text) }))) return
      pending.value = true
      try {
        const forked = await chat.fork(id, row.cut)
        void sessions.refresh()
        await router.push({ name: "chat-session", params: { sessionId: forked } })
      } finally {
        pending.value = false
      }
    },
    async branch(messageId) {
      sessionId("branch")
      const row = userRow(messageId)
      input.openBranch(row.text, row.cut)
    },
    async edit() {
      unavailable("chatTimeline.unavailable.edit")
    },
    async approve(requestId, decision) {
      const id = sessionId("approve")
      const approval = chat.views[id]!.approvals.find(request => request.branchId === input.branchId.value && request.requestId === requestId)
      if (approval === undefined) unavailable("chatTimeline.unavailable.request")
      await chat.respond(id, approval, decision)
    },
    async answer(requestId, answers) {
      const id = sessionId("answer")
      const question = chat.views[id]!.questions.find(ask => ask.branchId === input.branchId.value && ask.requestId === requestId)
      if (question === undefined) unavailable("chatTimeline.unavailable.request")
      await chat.answerQuestion(id, question, answers.map(answer => questionAnswer(question, answer)))
    },
    openFile() {
      toast.push({ tone: "info", title: t("chatTimeline.unavailable.openFile") })
    },
    copy: copyText,
  }

  const session = computed(() => {
    const id = input.sessionId.value
    if (id === undefined) return null
    const messages: ChatUiMessage[] = []
    const timeline: ChatUiTimelineEntry[] = []
    for (const row of input.rows.value) {
      if (row.kind === "turn") {
        if (row.turn.userMessage !== null) messages.push(row.turn.userMessage)
        timeline.push({ kind: "turn", key: row.key, turn: sessionTurn(row.turn) })
      } else if (row.kind === "message") {
        messages.push(row.message)
        timeline.push({ kind: "message", key: row.key, message: row.message })
      }
    }
    return {
      sessionId: id,
      status: "ready" as const,
      messages,
      timeline,
      running: input.running.value,
      stopping: false,
      pendingApprovals: [],
      pendingQuestions: [],
    }
  })
  const sessionIdOrNull = computed(() => input.sessionId.value ?? null)
  const host: ChatUiHost = {
    session,
    capabilities,
    commands,
    i18n,
    timeline: {
      scope: { sessionId: input.sessionId, branchId: input.branchId, detailSessionId: sessionIdOrNull },
      actions: {
        canEdit: () => false,
        canFork: messageId => actions.value.fork.available && input.rowByKey(messageId)?.kind === "user",
        pending,
      },
      slots: { turnOutput: WebNoticeOutput },
      hooks: {
        isSystemNoticeMessage: message => message.role === "system",
        systemNoticeText: message => input.noticeText(message.id),
      },
      effects: {
        notify: {
          success: title => toast.push({ tone: "success", title }),
          error: title => toast.push({ tone: "danger", title }),
        },
      },
    },
  }
  return { host, actions }
}
