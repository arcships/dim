/**
 * Folds one branch's events into the timeline rows the chat page draws,
 * following the TUI's `crates/dim-cli/src/projection.rs`:
 *
 * - events apply once, in sequence order; a replayed or duplicate one is skipped;
 * - reasoning and answer text stream into one row per model request, and the
 *   unwritten tail from `session.model.progress` shows until committed deltas
 *   overtake it; a stream settles on what the Timeline holds;
 * - a tool row opens on `step/tool-use` and completes on its result.
 *
 * Rows are immutable values: a change replaces the row, so a published copy of
 * `rows` re-renders only the rows that changed.
 */
import { GoalFold, type Goal } from "./goal"
import type { AskQuestion, ModelId, ModelProgress, QueuedInput, SessionEvent, ToolResult } from "./rpc/protocol"

export type TimelineRow =
  | { kind: "user"; key: string; text: string; at: number; sequence: number; cut: number }
  | { kind: "assistant"; key: string; text: string; streaming: boolean }
  | { kind: "thinking"; key: string; text: string; streaming: boolean; elapsedMs?: number }
  | { kind: "tool"; key: string; callId: string; name: string; input: Record<string, unknown>; pending: boolean; result?: ToolResult }
  | { kind: "line"; key: string; branchId: string; purpose: string; before?: string; failed?: boolean }
  | { kind: "line-ended"; key: string; issuer: "itself" | "operation" | "ancestor"; ancestor?: string }
  | { kind: "source-message"; key: string; from: string; steer: boolean; final: boolean; body: string }
  | { kind: "source-idle"; key: string; branch: string; status: string }
  | { kind: "source-ended"; key: string; branch: string }
  | { kind: "error"; key: string; text: string }
  | { kind: "stopped"; key: string }

/** A request's reasoning or answer while it is still arriving. */
type Stream = {
  index: number
  /** The request's committed deltas so far. */
  written: string
  /** Unicode scalar values in `written`, the unit of progress offsets. */
  writtenChars: number
  /** The latest unwritten tail and the offset it follows. */
  progress?: { text: string; offset: number }
}

export class Projection {
  rows: TimelineRow[] = []
  /** Highest event sequence applied. */
  cursor = 0
  /** The newest turn end on this line, the cut `/branch` and `+` use. */
  lastTurnEnd: number | undefined
  /** The model the branch last adopted (`context/configured`). */
  model: ModelId | undefined
  private answers = new Map<string, Stream>()
  private thoughts = new Map<string, Stream>()
  private requestStarts = new Map<string, number>()
  private tools = new Map<string, number>()
  private lines = new Map<string, number>()
  /** `ask_user` calls whose batch has no `step/tool-result` yet, oldest first. */
  private asks: Array<{ requestId: string; callId: string; questions: AskQuestion[] }> = []
  /** Messages the server holds in the queue: written, not yet adopted, not withdrawn. */
  private held = new Map<string, QueuedInput>()
  /** The branch's goal, folded from its events (goal.ts). */
  readonly goalFold = new GoalFold()

  /** The goal in force on this branch. */
  get goal(): Goal | undefined {
    return this.goalFold.goal()
  }

  /** The messages still queued on this branch, oldest first. */
  queued(): readonly QueuedInput[] {
    return [...this.held.values()]
  }

  /** The waiting `ask_user` calls of this branch: named by batch `requestId` and call id. */
  waiting(): readonly { requestId: string; callId: string; questions: AskQuestion[] }[] {
    return this.asks
  }

  /** Applies one event; false when it was already applied. */
  apply(event: SessionEvent): boolean {
    if (event.sequence <= this.cursor) return false
    this.cursor = event.sequence
    this.goalFold.apply(event)
    const content = event.content
    switch (content.kind) {
      case "input":
        if (content.user !== undefined) this.held.set(event.id, { id: event.id, text: content.text ?? "", user: content.user })
        if (content.text !== undefined) {
          this.push({
            kind: "user",
            key: `user-${event.sequence}`,
            text: content.text,
            at: event.createAt,
            sequence: event.sequence,
            cut: Math.max(0, event.sequence - 1),
          })
        }
        break
      case "input-applied":
      case "input-deleted":
        this.held.delete(content.inputId)
        break
      case "source-input": {
        const notice = branchNotice(content.text, content.mode)
        if (notice !== undefined) this.push({ ...notice, key: `notice-${event.sequence}` })
        break
      }
      case "branch-create": {
        const at = content.at ?? event.sequence
        const message = this.rows.find((row) => row.kind === "user" && row.cut === at)
        const before = message?.kind === "user" ? clipFirstLine(message.text) : undefined
        this.lines.set(content.branchId, this.push({
          kind: "line",
          key: `line-${event.sequence}`,
          branchId: content.branchId,
          purpose: firstLine(content.prompt),
          ...(before === undefined ? {} : { before }),
        }))
        break
      }
      case "branch-create-failed": {
        const index = this.lines.get(content.branchId)
        if (index !== undefined) this.update(index, { failed: true })
        break
      }
      case "branch-end": {
        const ancestor = content.ancestor
        this.push({
          kind: "line-ended",
          key: `line-ended-${event.sequence}`,
          issuer: ancestor !== undefined ? "ancestor" : content.source.kind === "tool" ? "itself" : "operation",
          ...(ancestor === undefined ? {} : { ancestor }),
        })
        break
      }
      case "model-start":
        this.requestStarts.set(content.requestId, event.createAt)
        break
      case "thinking": {
        const thought = this.openThinking(content.requestId)
        commit(thought, content.delta)
        const start = this.requestStarts.get(content.requestId)
        this.update(thought.index, { text: shown(thought), ...(start === undefined ? {} : { elapsedMs: Math.max(0, event.createAt - start) }) })
        break
      }
      case "text": {
        this.closeThinking(content.requestId)
        const answer = this.openAnswer(content.requestId)
        commit(answer, content.delta)
        this.update(answer.index, { text: shown(answer) })
        break
      }
      case "tool-use":
        this.closeThinking(content.requestId)
        this.closeAnswer(content.requestId)
        this.requestStarts.delete(content.requestId)
        this.tools.set(content.callId, this.push({ kind: "tool", key: `tool-${content.callId}`, callId: content.callId, name: content.name, input: content.input, pending: true }))
        break
      case "tool-call":
        for (const ask of content.asks) this.asks.push({ requestId: content.requestId, callId: ask.callId, questions: ask.questions })
        break
      case "tool-completed":
        this.completeTool(content.callId, content.result)
        break
      case "tool-result":
        for (const result of content.results) this.completeTool(result.callId, result.result)
        this.asks = this.asks.filter((ask) => ask.requestId !== content.requestId || !content.results.some((result) => result.callId === ask.callId))
        break
      case "model-end":
        this.closeThinking(content.requestId)
        this.closeAnswer(content.requestId)
        this.requestStarts.delete(content.requestId)
        if (content.error !== undefined) this.push({ kind: "error", key: `error-${event.sequence}`, text: content.error })
        break
      case "turn-end":
        this.lastTurnEnd = event.sequence
        for (const requestId of [...this.thoughts.keys()]) this.closeThinking(requestId)
        for (const requestId of [...this.answers.keys()]) this.closeAnswer(requestId)
        this.requestStarts.clear()
        if (content.outcome === "failed") this.push({ kind: "error", key: `error-${event.sequence}`, text: content.error ?? "" })
        if (content.outcome === "cancelled") this.push({ kind: "stopped", key: `stopped-${event.sequence}` })
        break
      case "context-configured":
        this.model = content.model
        break
      case "other":
        break
    }
    return true
  }

  /** Shows a request's output that is not written yet. */
  progress(progress: ModelProgress): void {
    const requestId = progress.requestId
    if (requestId === undefined) return
    if (progress.thinking !== "") {
      const thought = this.openThinking(requestId)
      thought.progress = { text: progress.thinking, offset: progress.thinkingOffset }
      this.update(thought.index, { text: shown(thought) })
    }
    if (progress.text !== "") {
      const answer = this.openAnswer(requestId)
      answer.progress = { text: progress.text, offset: progress.textOffset }
      this.update(answer.index, { text: shown(answer) })
    }
  }

  private push(row: TimelineRow): number {
    this.rows.push(row)
    return this.rows.length - 1
  }

  private update(index: number, patch: Partial<TimelineRow>): void {
    this.rows[index] = { ...this.rows[index]!, ...patch } as TimelineRow
  }

  private openThinking(requestId: string): Stream {
    let thought = this.thoughts.get(requestId)
    if (thought === undefined) {
      thought = { index: this.push({ kind: "thinking", key: `thinking-${requestId}-${this.rows.length}`, text: "", streaming: true }), written: "", writtenChars: 0 }
      this.thoughts.set(requestId, thought)
    }
    return thought
  }

  private openAnswer(requestId: string): Stream {
    let answer = this.answers.get(requestId)
    if (answer === undefined) {
      answer = { index: this.push({ kind: "assistant", key: `assistant-${requestId}-${this.rows.length}`, text: "", streaming: true }), written: "", writtenChars: 0 }
      this.answers.set(requestId, answer)
    }
    return answer
  }

  /** A thought settles when the request moves on: progress never written (an interrupted thought) goes. */
  private closeThinking(requestId: string): void {
    const thought = this.thoughts.get(requestId)
    if (thought === undefined) return
    this.thoughts.delete(requestId)
    this.update(thought.index, { text: thought.written, streaming: false })
  }

  private closeAnswer(requestId: string): void {
    const answer = this.answers.get(requestId)
    if (answer === undefined) return
    this.answers.delete(requestId)
    this.update(answer.index, { text: answer.written, streaming: false })
  }

  private completeTool(callId: string, result: ToolResult): void {
    const index = this.tools.get(callId)
    if (index === undefined) return
    this.tools.delete(callId)
    this.update(index, { pending: false, result })
  }
}

type BranchNotice =
  | { kind: "source-message"; from: string; steer: boolean; final: boolean; body: string }
  | { kind: "source-idle"; branch: string; status: string }
  | { kind: "source-ended"; branch: string }

function branchNotice(text: string, mode: string | undefined): BranchNotice | undefined {
  if (text.startsWith("<dim-branch-message")) {
    const from = attr(text, "from")
    if (from === undefined) return undefined
    return {
      kind: "source-message",
      from,
      steer: mode === "steer",
      final: attr(text, "final") !== undefined,
      body: oneLine(unescape(elementBody(text, "</dim-branch-message>"))),
    }
  }
  if (text.startsWith("<dim-branch-idle")) {
    const branch = attr(text, "branch")
    if (branch === undefined) return undefined
    return { kind: "source-idle", branch, status: attr(text, "status") ?? "idle" }
  }
  if (text.startsWith("<dim-branch-ended")) {
    const branch = attr(text, "branch")
    return branch === undefined ? undefined : { kind: "source-ended", branch }
  }
  return undefined
}

function firstLine(text: string): string {
  return text.split("\n")[0]?.trim() ?? ""
}

function clipFirstLine(text: string): string {
  const line = firstLine(text)
  const characters = [...line]
  return characters.length <= 40 ? line : `${characters.slice(0, 40).join("")}…`
}

function oneLine(text: string): string {
  return text.split(/\s+/u).filter(Boolean).join(" ")
}

function unescape(text: string): string {
  return text.replaceAll("&lt;", "<").replaceAll("&gt;", ">").replaceAll("&amp;", "&")
}

function attr(text: string, name: string): string | undefined {
  const needle = `${name}="`
  const start = text.indexOf(needle)
  if (start < 0) return undefined
  const from = start + needle.length
  const end = text.indexOf("\"", from)
  return end < 0 ? undefined : text.slice(from, end)
}

function elementBody(text: string, close: string): string {
  const newline = text.indexOf("\n")
  const body = newline < 0 ? "" : text.slice(newline + 1)
  const end = body.indexOf(close)
  return end < 0 ? body : body.slice(0, end)
}

function commit(stream: Stream, delta: string): void {
  stream.written += delta
  stream.writtenChars += [...delta].length
}

/**
 * Committed text plus the part of the latest progress it has not overtaken.
 * Progress that runs ahead of the committed deltas waits for the next notice.
 */
function shown(stream: Stream): string {
  const progress = stream.progress
  if (progress === undefined) return stream.written
  const overlap = stream.writtenChars - progress.offset
  if (overlap < 0) return stream.written
  const tail = [...progress.text]
  if (overlap >= tail.length) {
    stream.progress = undefined
    return stream.written
  }
  return stream.written + tail.slice(overlap).join("")
}
