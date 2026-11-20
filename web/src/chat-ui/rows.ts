import type {
  ChatUiAttachment,
  ChatUiTimelineBlock,
  ChatUiTimelineRow,
  ChatUiTimelineRowBlock,
  ChatUiTimelineRowMessage,
  ChatUiTimelineRowTurn,
  ChatUiTurnTerminalState,
} from "@dimcode/chat-ui"
import type { TimelineRow } from "../projection"

/** Web rows the shared timeline shows as system notices, one `message` row each. */
type NoticeRow = Extract<TimelineRow, { kind: "line" | "line-ended" | "source-message" | "source-idle" | "source-ended" | "hook-follow-up" }>
type Output = Extract<TimelineRow, { kind: "assistant" | "thinking" | "tool" }>
type TurnMember = Output | Extract<TimelineRow, { kind: "error" | "stopped" }>
type UserRow = Extract<TimelineRow, { kind: "user" }>

/** A block both the row renderers (`ChatUiTimelineRowBlock`) and the session view (`ChatUiTimelineBlock`) accept; the tool card reads the tool fields. */
type Block = ChatUiTimelineRowBlock & ChatUiTimelineBlock
/** The shared timeline's row, whose turn keeps the blocks' full type. */
export type WebChatRow =
  | Exclude<ChatUiTimelineRow, { kind: "turn" }>
  | { kind: "turn"; key: string; turn: ChatUiTimelineRowTurn & { blocks: readonly Block[] } }

/** The Projection carries no time for notice rows; the package requires one and does not show it. */
const NOTICE_TIME = new Date(0)

function isNoticeRow(row: TimelineRow): row is NoticeRow {
  return row.kind === "line" || row.kind === "line-ended" || row.kind === "source-message"
    || row.kind === "source-idle" || row.kind === "source-ended" || row.kind === "hook-follow-up"
}

/** Where the Web serves a stored image of the Session. */
export function imageUrl(sessionId: string, fileId: string): string {
  return `/api/sessions/${encodeURIComponent(sessionId)}/files/${encodeURIComponent(fileId)}`
}

function userMessage(row: UserRow, sessionId: string): ChatUiTimelineRowMessage {
  const at = new Date(row.at)
  const attachments: ChatUiAttachment[] | undefined = row.images?.map(image => ({
    id: image.source.fileId,
    name: image.source.fileId,
    type: "image",
    size: 0,
    mime: image.source.mediaType,
    url: imageUrl(sessionId, image.source.fileId),
  }))
  return {
    id: row.key,
    orderKey: String(row.sequence),
    role: "user",
    content: row.text,
    status: "complete",
    createdAt: at,
    updatedAt: at,
    ...(attachments === undefined ? {} : { attachments }),
  }
}

function block(row: Output): Block {
  switch (row.kind) {
    case "assistant":
      return { id: row.key, kind: "text", status: row.streaming ? "running" : "complete", content: row.text }
    case "thinking":
      return {
        id: row.key,
        kind: "thinking",
        status: row.streaming ? "running" : "complete",
        content: row.text,
        ...(row.elapsedMs === undefined ? {} : { durationMs: row.elapsedMs }),
      }
    case "tool":
      return {
        id: row.key,
        kind: "tool",
        status: row.pending ? "running" : row.result?.isError === true ? "error" : "complete",
        content: "",
        toolName: row.name,
        toolParams: row.input,
        toolCallId: row.callId,
        ...(row.result === undefined ? {} : { toolResult: { content: row.result.text, isError: row.result.isError } }),
      }
  }
}

function terminal(row: Extract<TurnMember, { kind: "error" | "stopped" }>): ChatUiTurnTerminalState {
  return row.kind === "stopped"
    ? { status: "stopped", reason: "user_interrupt", sourceMessageId: row.key }
    : { status: "failed", reason: "model_error", message: row.text, sourceMessageId: row.key }
}

function turn(user: UserRow | undefined, members: readonly TurnMember[], sessionId: string): WebChatRow {
  const last = members.at(-1)
  const ending = last?.kind === "error" || last?.kind === "stopped" ? last : undefined
  const blocks = members.filter((member): member is Output => member.kind !== "error" && member.kind !== "stopped").map(block)
  const id = user?.key ?? members[0]!.key
  const value = {
    id,
    userMessage: user === undefined ? null : userMessage(user, sessionId),
    messageIds: [...(user === undefined ? [] : [user.key]), ...members.map(member => member.key)],
    forkAnchorMessageId: null,
    forkAfterSequence: null,
    blocks,
    canShowWorkDuration: false,
    workDurationMs: null,
    turnStartedAt: user === undefined ? null : new Date(user.at),
    turnEndedAt: null,
    isRunning: blocks.some(item => item.status === "running"),
    ...(ending === undefined ? {} : { terminal: terminal(ending) }),
  } satisfies ChatUiTimelineRowTurn
  return { kind: "turn", key: id, turn: value }
}

function notice(row: NoticeRow): WebChatRow {
  return {
    kind: "message",
    key: row.key,
    message: { id: row.key, role: "system", content: "", status: "complete", createdAt: NOTICE_TIME, updatedAt: NOTICE_TIME },
  }
}

type Cached = { sessionId: string; members: readonly TimelineRow[]; row: WebChatRow }

/**
 * Maps Projection rows to the shared timeline's rows. A turn starts at each
 * `user` row and collects the output rows after it. A notice row ends the turn,
 * and so does an error row, which becomes the turn's terminal: the shared
 * renderer shows one terminal per turn, with its details, and hides status
 * blocks beside it. Output after either opens a turn without a user message. Projection rows are
 * replaced, never mutated, so a turn whose member rows are the same objects is
 * reused and only the streaming turn is rebuilt.
 */
export function createRowMapper(): (rows: readonly TimelineRow[], sessionId: string) => WebChatRow[] {
  let cache = new Map<string, Cached>()
  return (rows, sessionId) => {
    const next = new Map<string, Cached>()
    const result: WebChatRow[] = []
    let group: TimelineRow[] = []
    const flush = (): void => {
      if (group.length === 0) return
      const key = group[0]!.key
      const cached = cache.get(key)
      const reuse = cached !== undefined && cached.sessionId === sessionId
        && cached.members.length === group.length && cached.members.every((member, index) => member === group[index])
      const [first, ...rest] = group
      const row = reuse
        ? cached.row
        : isNoticeRow(first!)
          ? notice(first)
          : first!.kind === "user"
            ? turn(first, rest as TurnMember[], sessionId)
            : turn(undefined, group as TurnMember[], sessionId)
      next.set(key, { sessionId, members: group, row })
      result.push(row)
      group = []
    }
    for (const row of rows) {
      if (row.kind === "user" || isNoticeRow(row)) flush()
      group.push(row)
      if (isNoticeRow(row) || row.kind === "error") flush()
    }
    flush()
    cache = next
    return result
  }
}
