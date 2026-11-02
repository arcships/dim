/**
 * A branch's goal, folded from its own events, and the `/goal` forms. A port of
 * `crates/dim-cli/src/goal.rs` (Goal §2, A.3): the web client reads no goal
 * state from the App Server, so it folds the same goal blocks the TUI folds.
 */
import type { RawBlock, SessionEvent } from "./rpc/protocol"

/** The keys of the objective block and of the status block (Goal §A.2). */
const OBJECTIVE_KEY = "goal:objective"
const STATUS_KEY = "goal:status"
const MARKER = "<dim-goal "

export type GoalStatus =
  | { kind: "active" }
  | { kind: "paused"; reason: string }
  | { kind: "completed" }
  | { kind: "blocked" }
  | { kind: "none" }

/** The goal in force on one branch. */
export type Goal = { objective: string; status: GoalStatus; evaluator: boolean }

/** The method a writing request calls: `session.goal.<call>`. */
export type GoalCall = "set" | "edit" | "pause" | "resume" | "clear"

/** What `/goal` does with the words after it. */
export type GoalRequest =
  | { form: "show" }
  | { form: "objective"; objective: string }
  | { form: "set" | "edit"; objective: string }
  | { form: "pause" | "resume" | "clear" }

type Block =
  | { kind: "objective"; input: string; text: string | undefined; set: boolean; judge: string | undefined }
  | { kind: "objective-retire"; input: string }
  | { kind: "status"; input: string; status: GoalStatus; byEvaluator: boolean }
  | { kind: "status-retire"; input: string }

/**
 * The goal of one branch: the latest live block of each key decides, `clear`
 * retires both keys, `edit` keeps the judge, a deleted input's blocks leave the
 * fold, and a branch start or a fork restarts it.
 */
export class GoalFold {
  private blocks: Block[] = []
  private deleted = new Set<string>()

  apply(event: SessionEvent): void {
    const content = event.content
    switch (content.kind) {
      case "branch-start":
      case "session-forked":
        this.blocks = []
        break
      case "input":
      case "source-input":
        for (const block of content.blocks) this.add(event.id, block)
        break
      case "input-deleted":
        if (this.blocks.some((block) => block.input === content.inputId)) this.deleted.add(content.inputId)
        break
    }
  }

  goal(): Goal | undefined {
    let objective: string | undefined
    let judge = false
    let status: GoalStatus | undefined
    for (const block of this.blocks) {
      if (this.deleted.has(block.input)) continue
      switch (block.kind) {
        case "objective":
          objective = block.text
          if (block.set) judge = block.judge === "evaluator"
          break
        case "objective-retire":
          objective = undefined
          judge = false
          break
        case "status":
          // Under the evaluator judge a worker's `completed` is a request; only the evaluator completes the goal.
          if (block.status.kind === "completed" && !block.byEvaluator && judge) break
          status = block.status
          break
        case "status-retire":
          status = undefined
          break
      }
    }
    if (objective === undefined) return undefined
    return { objective, status: status ?? { kind: "none" }, evaluator: judge }
  }

  private add(input: string, block: RawBlock): void {
    const { text } = block
    if (block.key !== OBJECTIVE_KEY && block.key !== STATUS_KEY) return
    if (marker(text) === undefined) return
    if (block.retire) {
      this.blocks.push(block.key === OBJECTIVE_KEY ? { kind: "objective-retire", input } : { kind: "status-retire", input })
      return
    }
    if (block.key === OBJECTIVE_KEY) {
      this.blocks.push({
        kind: "objective",
        input,
        text: value(text, "Objective:"),
        set: attribute(text, "op") === "set",
        judge: attribute(text, "judge"),
      })
      return
    }
    const status = statusOf(text)
    if (status !== undefined) this.blocks.push({ kind: "status", input, status, byEvaluator: attribute(text, "by") === "evaluator" })
  }
}

/** The status a live `goal:status` block sets (Goal A.3). */
function statusOf(text: string): GoalStatus | undefined {
  if (attribute(text, "kind") === "status") return { kind: "active" }
  switch (attribute(text, "op")) {
    case "pause": return { kind: "paused", reason: attribute(text, "reason") ?? "user" }
    case "resume": return { kind: "active" }
    case "completed": return { kind: "completed" }
    case "blocked": return { kind: "blocked" }
    default: return undefined
  }
}

/** The text after a block's `label` line, up to its last blank line, with the marker's escaping reversed. */
function value(text: string, label: string): string | undefined {
  const start = text.indexOf(`\n${label}\n`)
  if (start === -1) return undefined
  const end = text.lastIndexOf("\n\n")
  if (end === -1 || end < start) return undefined
  return unescape(text.slice(start + label.length + 2, end))
}

function unescape(text: string): string {
  return text.replaceAll("&quot;", "\"").replaceAll("&gt;", ">").replaceAll("&lt;", "<").replaceAll("&amp;", "&")
}

/** The attributes of a block's leading `<dim-goal ...>` marker, when the block opens with one. */
function marker(text: string): string | undefined {
  if (!text.startsWith(MARKER)) return undefined
  const rest = text.slice(MARKER.length)
  const close = rest.indexOf(">")
  return close === -1 ? undefined : rest.slice(0, close)
}

/** The value of `name="value"` in the block's marker. */
function attribute(text: string, name: string): string | undefined {
  const attrs = marker(text)
  if (attrs === undefined) return undefined
  let offset = 0
  for (;;) {
    const eq = attrs.indexOf("=\"", offset)
    if (eq === -1) return undefined
    let start = 0
    for (let index = eq - 1; index >= 0; index--) {
      if (!/[A-Za-z0-9_]/.test(attrs[index]!)) {
        start = index + 1
        break
      }
    }
    const quote = eq + 2
    const end = attrs.indexOf("\"", quote)
    if (end === -1) return undefined
    if (attrs.slice(start, eq) === name) return unescape(attrs.slice(quote, end))
    offset = end + 1
  }
}

/** Reads `/goal`'s words the way the TUI does; `undefined` is a usage error. */
export function parseGoalRequest(words: string): GoalRequest | undefined {
  const trimmed = words.trim()
  if (trimmed === "") return { form: "show" }
  const space = trimmed.search(/\s/)
  const word = space === -1 ? trimmed : trimmed.slice(0, space)
  const rest = space === -1 ? "" : trimmed.slice(space).trim()
  switch (word) {
    case "pause":
    case "resume":
    case "clear":
      if (rest === "") return { form: word }
      break
    case "set":
    case "edit":
      return rest === "" ? undefined : { form: word, objective: rest }
  }
  return { form: "objective", objective: trimmed }
}

/** The objective a writing request carries. */
export function goalObjective(request: GoalRequest): string | undefined {
  switch (request.form) {
    case "show":
    case "pause":
    case "resume":
    case "clear":
      return undefined
    default:
      return request.objective
  }
}

/**
 * The call a request makes, given the goal in force. A typed objective edits
 * only a goal still being worked toward (active or paused); otherwise it sets
 * a new goal.
 */
export function goalCall(request: GoalRequest, goal: Goal | undefined): GoalCall | undefined {
  switch (request.form) {
    case "show": return undefined
    case "objective": return goal !== undefined && (goal.status.kind === "active" || goal.status.kind === "paused") ? "edit" : "set"
    default: return request.form
  }
}
