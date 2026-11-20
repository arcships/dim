import { computed, ref, type Ref } from "vue"
import type {
  ChatUiCapabilities,
  ChatUiCommands,
  ChatUiHost,
  ChatUiMessage,
  ChatUiTimelineEntry,
  ChatUiTimelineTurn,
} from "@dimcode/chat-ui"
import { copyText } from "../clipboard"
import { useToastStore } from "../stores/toast"
import { createChatUiI18n } from "./i18n"
import type { WebChatRow } from "./rows"
import WebNoticeOutput from "./WebNoticeOutput.vue"

export type WebChatUiHostInput = {
  sessionId: Readonly<Ref<string | undefined>>
  rows: Readonly<Ref<readonly WebChatRow[]>>
  running: Readonly<Ref<boolean>>
  /** Display text of a notice row (`rows.ts` maps every Web-only row kind to one). */
  noticeText: (messageId: string) => string
}

const READ_ONLY: ChatUiCapabilities = {
  interactive: false,
  terminal: false,
  browserPanel: false,
  filePreview: false,
  mcpApps: false,
  notifications: false,
  attachments: false,
}

const noop = async (): Promise<void> => {}

/** The row turn's terminal is the renderer's detail object; the session view names only the outcome. */
function sessionTurn(turn: Extract<WebChatRow, { kind: "turn" }>["turn"]): ChatUiTimelineTurn {
  const { terminal, ...rest } = turn
  const state = terminal === undefined ? (turn.isRunning ? undefined : "completed") : terminal.status === "failed" ? "failed" : "cancelled"
  return { ...rest, ...(state === undefined ? {} : { terminal: state }) }
}

/**
 * The read-only Web host. Rows come from the caller's Projection view through
 * `rows.ts`; commands are callable and do nothing except `copy`. Approvals and
 * questions stay in the Web prompts above the composer, so the session view
 * lists none.
 */
export function createWebChatUiHost(input: WebChatUiHostInput): ChatUiHost {
  const toast = useToastStore()
  const i18n = createChatUiI18n()
  const commands: ChatUiCommands = {
    send: noop,
    stop: noop,
    retry: noop,
    fork: noop,
    branch: noop,
    edit: noop,
    approve: noop,
    answer: noop,
    openFile: () => {},
    copy: copyText,
  }
  const session = computed(() => {
    const sessionId = input.sessionId.value
    if (sessionId === undefined) return null
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
      sessionId,
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
  return {
    session,
    capabilities: ref(READ_ONLY),
    commands,
    i18n,
    timeline: {
      scope: { sessionId: input.sessionId, detailSessionId: sessionIdOrNull },
      actions: { canEdit: () => false, canFork: () => false, pending: ref(false) },
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
}
