/**
 * Folds a session's main-branch events into the timeline rows the chat page
 * draws, following the TUI's `crates/dim-cli/src/projection.rs`:
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
import type { ModelId, ModelProgress, SessionEvent, ToolResult } from "./rpc/protocol"

export type TimelineRow =
  | { kind: "user"; key: string; text: string; at: number }
  | { kind: "assistant"; key: string; text: string; streaming: boolean }
  | { kind: "thinking"; key: string; text: string; streaming: boolean; elapsedMs?: number }
  | { kind: "tool"; key: string; callId: string; name: string; input: Record<string, unknown>; pending: boolean; result?: ToolResult }
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
  /** The model the branch last adopted (`context/configured`). */
  model: ModelId | undefined
  private answers = new Map<string, Stream>()
  private thoughts = new Map<string, Stream>()
  private requestStarts = new Map<string, number>()
  private tools = new Map<string, number>()

  /** Applies one event; false when it was already applied. */
  apply(event: SessionEvent): boolean {
    if (event.sequence <= this.cursor) return false
    this.cursor = event.sequence
    const content = event.content
    switch (content.kind) {
      case "input":
        if (content.text !== undefined) this.push({ kind: "user", key: `user-${event.sequence}`, text: content.text, at: event.createAt })
        break
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
      case "tool-completed":
        this.completeTool(content.callId, content.result)
        break
      case "tool-result":
        for (const result of content.results) this.completeTool(result.callId, result.result)
        break
      case "model-end":
        this.closeThinking(content.requestId)
        this.closeAnswer(content.requestId)
        this.requestStarts.delete(content.requestId)
        if (content.error !== undefined) this.push({ kind: "error", key: `error-${event.sequence}`, text: content.error })
        break
      case "turn-end":
        // A turn that ends mid-stream settles what the Timeline holds.
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
