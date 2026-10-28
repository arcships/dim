// Goal's language-neutral contract (docs/design/plugins/Goal.md): the change
// and continuation inputs, and the fold of a branch's local events.

export const OBJECTIVE_KEY = 'goal:objective'
export const STATUS_KEY = 'goal:status'

export type RawBlock = { type: 'raw'; text: string; key?: string; retire?: boolean }
export type InputMessage = {
  submission_id?: string
  user?: { content: { type: string; text?: string }[] }
  blocks?: RawBlock[]
  mode?: 'queue' | 'steer' | 'append'
}
export type AgentEvent = {
  id: string
  sequence: number | bigint
  type: string
  name: string
  data: Record<string, any>
}

export type Status = 'active' | 'paused' | 'completed' | 'blocked'
export type Judge = 'model' | 'evaluator'
export type PauseReason = 'user' | 'interrupted' | 'limit' | 'turn_failed' | 'evaluation_failed'

const REASONS: Record<Exclude<PauseReason, 'user'>, string> = {
  interrupted: 'the user interrupted the turn',
  limit: 'the automatic continuation limit was reached',
  turn_failed: 'the last turn failed',
  evaluation_failed: 'the evaluator could not check the result',
}

/**
 * Encodes text placed inside a marker body or attribute, so user and model
 * text cannot forge a marker (A.5, §7). The encoding is reversible: the fold
 * and clients read values back with `unescape`.
 */
export function escape(text: string): string {
  return text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;')
}
/** The inverse of `escape`. */
export function unescape(text: string): string {
  return text.replaceAll('&quot;', '"').replaceAll('&gt;', '>').replaceAll('&lt;', '<').replaceAll('&amp;', '&')
}
/**
 * The decoded value a change block carries after its `label` line (an
 * objective after `Objective:`, a summary after `Result:` or `Blocker:`): the
 * body up to the block's fixed closing text, which follows the last blank line.
 */
function value(text: string, label: string): string | undefined {
  const start = text.indexOf(`\n${label}\n`)
  const end = text.lastIndexOf('\n\n')
  if (start < 0 || end < start) return undefined
  return unescape(text.slice(start + label.length + 2, end))
}

/**
 * The decoded body a block carries after its `label` line, up to the block's
 * closing marker: for a body with no fixed text after it (Pursuit's report).
 */
function body(text: string, label: string): string {
  const start = text.indexOf(`\n${label}\n`)
  const end = text.lastIndexOf('\n</dim-goal>')
  if (start < 0 || end < start) return ''
  return unescape(text.slice(start + label.length + 2, end))
}

function block(attrs: Record<string, string | number>, body: string[], key?: string, retire = false): RawBlock {
  const marker = Object.entries(attrs).map(([name, value]) => `${name}="${escape(String(value))}"`).join(' ')
  const raw: RawBlock = { type: 'raw', text: `<dim-goal ${marker}>\n${body.join('\n')}\n</dim-goal>` }
  if (key) raw.key = key
  if (retire) raw.retire = true
  return raw
}

function input(blocks: RawBlock[], mode: 'queue' | 'append'): InputMessage {
  return { submission_id: `goal:${crypto.randomUUID()}`, blocks, mode }
}

function activeStatus(branch: string): RawBlock {
  return block({ kind: 'status', status: 'active', branch }, [
    `The goal for branch ${branch} is active. Keep working toward it across turns. When it is fully achieved, call goal_update with status "completed". If you cannot make progress without the user, call goal_update with status "blocked" and explain why.`,
  ], STATUS_KEY)
}

function cleared(branch: string, attrs: Record<string, string>, text: string): InputMessage {
  return input([
    block({ kind: 'goal', op: 'clear', ...attrs, branch }, [text], OBJECTIVE_KEY, true),
    block({ kind: 'status', status: 'cleared', branch }, [`The goal for branch ${branch} has no status.`], STATUS_KEY, true),
  ], 'append')
}

/** The change inputs (A.5); each is one `input/message`. */
export const changes = {
  set: (branch: string, objective: string, judge: Judge = 'model') => input([
    block({ kind: 'goal', op: 'set', ...(judge === 'evaluator' ? { judge } : {}), branch }, [
      'Objective:', escape(objective), '',
      `This is the user's standing goal for branch ${branch}. If you are working on a delegated sub-task started from branch ${branch}, treat this goal as background and do not call goal_update.`,
    ], OBJECTIVE_KEY),
    activeStatus(branch),
  ], 'queue'),
  edit: (branch: string, objective: string) => input([
    block({ kind: 'goal', op: 'edit', branch }, [
      'Objective:', escape(objective), '',
      `The user set the objective of the goal for branch ${branch}. This does not change its status.`,
    ], OBJECTIVE_KEY),
  ], 'append'),
  pause: (branch: string) => input([
    block({ kind: 'goal', op: 'pause', reason: 'user', branch }, [
      `The user set the goal status for branch ${branch} to paused. Do not continue working toward a goal for branch ${branch} on your own; respond only to the user's latest input.`,
    ], STATUS_KEY),
  ], 'append'),
  resume: (branch: string) => input([
    block({ kind: 'goal', op: 'resume', branch }, [
      `The user set the goal status for branch ${branch} to active. If branch ${branch} has a goal objective, keep working toward it across turns; when it is completed or blocked, call goal_update.`,
    ], STATUS_KEY),
  ], 'queue'),
  clear: (branch: string) => cleared(branch, {}, 'The previous goal was cleared by the user. Disregard it.'),
  report: (branch: string, status: 'completed' | 'blocked', summary: string) => input([
    block({ kind: 'goal', op: status, branch }, [
      status === 'completed' ? 'Result:' : 'Blocker:', escape(summary), '',
      `goal_update set the goal status for branch ${branch} to ${status}. Do not continue working toward a goal for branch ${branch} on your own.`,
    ], STATUS_KEY),
  ], 'append'),
}

/** Goal's own writes, each marked with the sequence that decided it. */
export const writes = {
  continuation: (branch: string, count: number, limit: number, after: bigint) => input([
    block({ kind: 'continuation', count, limit, after: String(after), branch }, [
      `Continue working toward the active goal for branch ${branch}. If it is completed or blocked, call goal_update. If you are working on a delegated sub-task started from branch ${branch}, do not call goal_update.`,
    ]),
  ], 'queue'),
  pause: (branch: string, reason: Exclude<PauseReason, 'user'>, after: bigint) => input([
    block({ kind: 'goal', op: 'pause', reason, after: String(after), branch }, [
      `The goal is paused (${REASONS[reason]}). Do not continue working toward it on your own; respond only to the user's latest input.`,
    ], STATUS_KEY),
  ], 'append'),
  startClear: (branch: string, after: bigint) => cleared(branch, { reason: 'new_branch', after: String(after) },
    `Any goal above belongs to another branch, not to branch ${branch}. Disregard it; branch ${branch} has no goal.`),
  // Pursuit's writes (§5): the follow-up on the evaluator, and the conclusion,
  // outcome and continuation on the working branch.
  evaluateAgain: (branch: string, after: bigint) => input([
    block({ kind: 'evaluate-again', after: String(after), branch }, [
      'Your reply did not end with a conclusion. End your reply with exactly one of DONE:, CONTINUE: or BLOCKED: as described above.',
    ]),
  ], 'queue'),
  conclusion: (branch: string, evaluator: string, evaluation: string, result: ConclusionKind, text: string) => input([
    block({ kind: 'report', result, evaluation, evaluator, branch }, ['Evaluator report:', escape(text)]),
  ], 'append'),
  evaluated: (branch: string, op: 'completed' | 'blocked', evaluation: string, text: string) => input([
    block({ kind: 'goal', op, by: 'evaluator', evaluation, branch }, [
      op === 'completed' ? 'Result:' : 'Blocker:', escape(text), '',
      op === 'completed'
        ? `An evaluator verified that the goal for branch ${branch} is completed. Do not continue working toward it on your own.`
        : `An evaluator found that the goal for branch ${branch} cannot proceed without the user. Do not continue working toward it on your own.`,
    ], STATUS_KEY),
  ], 'append'),
  evaluatedContinuation: (branch: string, count: number, limit: number, after: bigint, evaluation: string) => input([
    block({ kind: 'continuation', count, limit, after: String(after), evaluation, branch }, [
      `An evaluator checked your report that the goal for branch ${branch} is completed and found it is not achieved yet. Its report above lists items that are at least still missing; the list may be incomplete. Assess yourself how far the goal still is, then continue working toward it. When you think it is achieved, call goal_update with status "completed" again.`,
    ]),
  ], 'queue'),
}

/** The result a conclusion states, and the evaluator's report of a failed turn. */
export type ConclusionKind = 'done' | 'continue' | 'blocked' | 'failed'
/** A conclusion input's attributes and decoded text. */
export type Conclusion = { evaluation: string; evaluator: string; result: ConclusionKind; text: string }

/**
 * A sequence as Goal writes one (`String` of a `bigint`, in `after` and
 * `evaluation`): decimal, without a leading zero. Anything else a marker or an
 * operation id carries names no evaluation (§5).
 */
function sequenceOf(value: string | undefined): string | undefined {
  return value !== undefined && /^[1-9]\d*$/.test(value) ? value : undefined
}

/** The operation id a `branch/create` records for evaluation `N` (§4.1). */
export const evaluationOperation = (evaluation: string) => `goal:evaluate:${evaluation}`

/** The evaluation a `branch/create` operation id names, if it is one. */
export function evaluationOfOperation(operation: string | undefined): string | undefined {
  const prefix = 'goal:evaluate:'
  return operation?.startsWith(prefix) ? sequenceOf(operation.slice(prefix.length)) : undefined
}

const CONCLUSION = 4000
const PREFIX: Record<ConclusionKind, string> = { done: 'DONE:', continue: 'CONTINUE:', blocked: 'BLOCKED:', failed: '' }

/** Text cut to the 4000 characters a conclusion or a whole reply keeps (§4.2). */
export function truncate(text: string): string {
  return [...text].slice(0, CONCLUSION).join('')
}

/**
 * The evaluator's prompt, the first Queue input of its branch (§4.2, §5). Its
 * first line is the marker that gives Goal the evaluator role.
 */
export function evaluatorPrompt(branch: string, evaluation: string, objective: string, summary: string): string {
  return [
    `<dim-goal kind="evaluate" branch="${escape(branch)}" evaluation="${evaluation}">`,
    `You are an evaluator. Branch ${branch} reported that its goal is completed. Only you decide whether it is. Do not continue the work, do not change files or settings, and do not call goal_update.`,
    '',
    'Objective:',
    escape(objective),
    '',
    'Reported result:',
    escape(summary),
    '',
    'Verify the facts yourself by reading files and running checks; do not rely on the report or on earlier messages. End your reply with exactly one of:',
    'DONE: <the verified result>',
    'CONTINUE: followed by the items that are at least still missing, one per line, each with its evidence',
    'BLOCKED: <what only the user can provide>',
  ].join('\n')
}

/**
 * The conclusion an evaluator reply ends with: the reply's last line that
 * begins `DONE:`, `CONTINUE:` or `BLOCKED:`, to the end (§4.2).
 */
export function replyConclusion(reply: string): { result: ConclusionKind; text: string } | undefined {
  const lines = reply.split('\n')
  for (let index = lines.length - 1; index >= 0; index--) {
    const result = (['done', 'continue', 'blocked'] as const).find(kind => lines[index].startsWith(PREFIX[kind]))
    if (result) return { result, text: truncate(lines.slice(index).join('\n')) }
  }
}

/**
 * The conclusion a report block carries (§5). A report is a persisted
 * cross-plugin input, so every attribute it needs is checked here: a kind, one
 * of the four results, a decimal evaluation and a non-empty evaluator.
 */
export function conclusionOf(text: string): Conclusion | undefined {
  const attrs = marker(text) ?? {}
  const result = attrs.result as ConclusionKind | undefined
  const evaluation = sequenceOf(attrs.evaluation)
  if (attrs.kind !== 'report' || result === undefined || !Object.hasOwn(PREFIX, result)) return undefined
  if (evaluation === undefined || !attrs.evaluator) return undefined
  return { evaluation, evaluator: attrs.evaluator, result, text: body(text, 'Evaluator report:') }
}

/** The result a `done` or `blocked` conclusion states, without its prefix. */
export function conclusionResult(conclusion: Conclusion): string {
  return conclusion.text.slice(PREFIX[conclusion.result].length).trim()
}

/** The attributes of a block's leading `<dim-goal ...>` marker, if any. */
export function marker(text: string): Record<string, string> | undefined {
  const match = /^<dim-goal ([^>]*)>/.exec(text)
  if (!match) return undefined
  const attrs: Record<string, string> = {}
  for (const [, name, value] of match[1].matchAll(/(\w+)="([^"]*)"/g)) attrs[name] = unescape(value)
  return attrs
}

const seq = (event: AgentEvent) => BigInt(event.sequence)

/**
 * The event a branch's own history starts at: its `branch/start`, or the
 * `session/forked` a fork writes after the facts it copied. Events before it
 * are inherited or copied and never contribute a goal (§2, A11).
 */
export function isOpening(event: AgentEvent): boolean {
  return (event.type === 'branch' && event.name === 'start') || (event.type === 'session' && event.name === 'forked')
}

/** The status a live `goal:status` block sets. */
function statusOf(attrs: Record<string, string>, text: string): { status: Status; reason?: string; summary?: string } {
  if (attrs.kind === 'status') return { status: 'active' }
  switch (attrs.op) {
    case 'pause': return { status: 'paused', reason: attrs.reason }
    case 'resume': return { status: 'active' }
    default: {
      return { status: attrs.op as Status, summary: value(text, attrs.op === 'completed' ? 'Result:' : 'Blocker:') }
    }
  }
}

/** One branch's goal state: the fold of its consumed local events (§2). */
export class Fold {
  objective: string | undefined
  status: { status: Status; reason?: string; summary?: string } | undefined
  /** Sequence of the latest change that set `active` (`set` or `resume`). */
  active = -1n
  lastOperator = -1n
  turnStart = -1n
  turnEnd: { sequence: bigint; status: string } | undefined
  /** Sequences of local goal blocks; any means the branch has its own goal history. */
  localBlocks = 0
  /**
   * At a `session/forked` opening: whether a goal key the copied facts left
   * was live. The copied facts are this branch's own events, so the fold
   * already knows it; undefined at a `branch/start`, whose prefix is
   * inherited and read from history.
   */
  copiedLive: boolean | undefined
  /**
   * The judge of the latest `set`, kept by `edit` (Pursuit §3): a `judge="evaluator"`
   * `set` marks it, and any other objective block leaves it as it was.
   */
  judge: Judge = 'model'
  /**
   * Under the evaluator judge, the worker's `completed` blocks: a request, not
   * a status change (A16). Each carries the summary the evaluator is shown.
   */
  requests: { sequence: bigint; summary: string }[] = []
  /** Evaluations created on this branch: `N` → the evaluator branch it names. */
  evaluations = new Map<string, string>()
  /** The first conclusion input from each evaluation's own evaluator: `N` → its sequence. */
  conclusions = new Map<string, bigint>()
  /** Sequence of the latest live keyed goal block: one after `N` supersedes `N`. */
  lastBlock = -1n
  /** The lines this branch opened that ended, by the `branch/ended` it records. */
  ended = new Set<string>()
  /** The lines this branch could not create, by their `branch/create-failed`. */
  failed = new Set<string>()
  /** Turn ends in this branch's local events, up to the latest. */
  turnEnds = 0
  private interrupts: bigint[] = []
  private continuations: bigint[] = []
  private decided = new Set<string>()
  private driving = new Map<string, bigint>()
  private events: AgentEvent[] = []
  private deleted = new Set<string>()

  /** Applies one consumed local event; `input/deleted` refolds without the input. */
  apply(event: AgentEvent): void {
    // The fold starts over at the branch's own opening; the events before it
    // are inherited or copied and never contribute a goal (§2, A11). Emptying
    // the event list here is what makes a refold replay from the opening.
    if (isOpening(event)) {
      const copiedLive = event.type === 'session' ? this.objective !== undefined || this.status !== undefined : undefined
      Object.assign(this, new Fold())
      this.copiedLive = copiedLive
    }
    if (event.type === 'input' && event.name === 'deleted') {
      this.deleted.add(event.data.input_id)
      this.events.push(event)
      this.refold()
      return
    }
    this.events.push(event)
    this.step(event)
  }

  private refold(): void {
    const { events, copiedLive } = this
    Object.assign(this, new Fold())
    this.events = events
    this.copiedLive = copiedLive
    this.deleted = new Set(events.filter(e => e.type === 'input' && e.name === 'deleted').map(e => e.data.input_id))
    for (const event of events) this.step(event)
  }

  private step(event: AgentEvent): void {
    const sequence = seq(event)
    const key = `${event.type}/${event.name}`
    switch (key) {
      case 'input/message': {
        if (this.deleted.has(event.id)) return
        const data = event.data as InputMessage
        const blocks = data.blocks ?? []
        const mode = data.mode ?? (data.user ? 'queue' : 'append')
        if (mode !== 'append') this.driving.set(event.id, sequence)
        const first = blocks[0] && marker(blocks[0].text)
        if (data.user && !(blocks[0]?.text.startsWith('<dim-'))) this.lastOperator = sequence
        if (first?.after) this.decided.add(first.after)
        if (first?.kind === 'continuation') this.continuations.push(sequence)
        // The validated parse the branch part decides with (§5): a report
        // missing an attribute matches no evaluation, not evaluation undefined.
        const conclusion = blocks[0] && conclusionOf(blocks[0].text)
        if (conclusion && this.evaluations.get(conclusion.evaluation) === conclusion.evaluator) {
          if (!this.conclusions.has(conclusion.evaluation)) this.conclusions.set(conclusion.evaluation, sequence)
        }
        for (const raw of blocks) {
          const attrs = marker(raw.text)
          if (!attrs || (raw.key !== OBJECTIVE_KEY && raw.key !== STATUS_KEY)) continue
          this.localBlocks++
          this.lastBlock = sequence
          if (raw.key === OBJECTIVE_KEY) {
            if (raw.retire) {
              this.objective = undefined
              this.judge = 'model'
            } else {
              this.objective = value(raw.text, 'Objective:')
              if (attrs.op === 'set') this.judge = attrs.judge === 'evaluator' ? 'evaluator' : 'model'
            }
            continue
          }
          if (raw.retire) {
            this.status = undefined
            continue
          }
          const status = statusOf(attrs, raw.text)
          if (status.status === 'completed' && attrs.by !== 'evaluator' && this.judge === 'evaluator') {
            // A16: under the evaluator judge the worker's `completed` is a
            // request; only the evaluator completes the goal (§4.1).
            this.requests.push({ sequence, summary: status.summary ?? '' })
            continue
          }
          this.status = status
          if (status.status === 'active' && (attrs.op === 'resume' || attrs.kind === 'status')) {
            this.active = sequence
          }
        }
        return
      }
      case 'branch/create': {
        const data = event.data as { branch: string; source?: { kind?: string; operation_id?: string } }
        const evaluation = data.source?.kind === 'host' ? evaluationOfOperation(data.source.operation_id) : undefined
        if (evaluation === undefined) return
        this.evaluations.set(evaluation, data.branch)
        this.decided.add(evaluation)
        return
      }
      case 'branch/ended':
        this.ended.add(event.data.branch)
        return
      case 'branch/create-failed':
        this.failed.add(event.data.branch)
        return
      case 'input/consumed':
        this.driving.delete(event.data.input_id)
        return
      case 'turn/start':
        this.turnStart = sequence
        return
      case 'turn/end':
        this.turnEnd = { sequence, status: event.data.status }
        this.turnEnds++
        return
      case 'stop/interrupt':
        if (event.data.scope === 'turn') this.interrupts.push(sequence)
        return
    }
  }

  /** A goal exists and is active (C1). */
  get running(): boolean {
    return this.objective !== undefined && this.status?.status === 'active'
  }
  get interrupted(): boolean {
    return this.interrupts.some(sequence => sequence > this.active)
  }
  get count(): number {
    const since = this.active > this.lastOperator ? this.active : this.lastOperator
    return this.continuations.filter(sequence => sequence > since).length
  }
  get pendingDriving(): boolean {
    return [...this.driving.keys()].some(id => !this.deleted.has(id))
  }
  get turnOpen(): boolean {
    return this.turnStart > (this.turnEnd?.sequence ?? -1n)
  }
  /** The worker's completion request inside the turn that ends at `sequence` (§4.1). */
  requestInTurn(sequence: bigint): { sequence: bigint; summary: string } | undefined {
    return this.requests.filter(request => request.sequence > this.turnStart && request.sequence < sequence).at(-1)
  }
  /** The evaluation the branch `evaluator` was created for, if it is one of ours. */
  evaluationOf(evaluator: string): string | undefined {
    for (const [evaluation, branch] of this.evaluations) if (branch === evaluator) return evaluation
  }
  /** Whether evaluation `N` is still current: no goal block follows it (§4.1). */
  isCurrent(evaluation: string): boolean {
    return this.lastBlock <= BigInt(evaluation)
  }
  isDecided(sequence: bigint): boolean {
    return this.decided.has(String(sequence))
  }
}

export type Decision =
  | { write: 'continuation'; count: number }
  | { write: 'pause'; reason: Exclude<PauseReason, 'user'> }
  | undefined

/**
 * The decision for the latest `turn/end` (A.4), over the fold as consumed. The
 * evaluator decision, when it applies, comes first: `decideEvaluation` takes
 * it before the caller reads the continuation limit.
 */
export function decideTurnEnd(fold: Fold, limit: number): Decision {
  const end = fold.turnEnd
  if (!end || fold.isDecided(end.sequence) || !fold.running) return undefined
  if (fold.interrupted) return { write: 'pause', reason: 'interrupted' }
  if (end.status === 'failed') return { write: 'pause', reason: 'turn_failed' }
  if (end.status !== 'completed' || fold.turnOpen || fold.pendingDriving) return undefined
  if (fold.count >= limit) return { write: 'pause', reason: 'limit' }
  return { write: 'continuation', count: fold.count + 1 }
}

/** The decision for a `stop/interrupt {scope: turn}` at `sequence`. */
export function decideInterrupt(fold: Fold, sequence: bigint): Decision {
  if (fold.isDecided(sequence) || !fold.running || fold.active >= sequence) return undefined
  return { write: 'pause', reason: 'interrupted' }
}

/**
 * Whether the turn ending at `sequence` creates an evaluator instead of
 * continuing (Pursuit §4.1): Goal's C1–C4 hold under the evaluator judge and
 * the turn carries the worker's completion request. It needs no Settings, so
 * the caller takes it before reading the continuation limit.
 */
export function decideEvaluation(fold: Fold, sequence: bigint): boolean {
  const end = fold.turnEnd
  if (!end || end.sequence !== sequence || fold.isDecided(sequence) || !fold.running) return false
  if (fold.interrupted || end.status !== 'completed' || fold.turnOpen) return false
  return fold.judge === 'evaluator' && fold.requestInTurn(sequence) !== undefined
}

export type ConclusionAction =
  | { write: 'completed' | 'blocked'; text: string }
  | { write: 'continue' | 'failed' }

/**
 * The action a conclusion input at `sequence` takes (Pursuit §4.1): the first
 * conclusion of a current evaluation, from the branch its `branch/create`
 * names. A later conclusion, another branch's, or one for a superseded
 * evaluation decides nothing (§8).
 */
export function decideConclusion(fold: Fold, sequence: bigint, conclusion: Conclusion): ConclusionAction | undefined {
  if (fold.conclusions.get(conclusion.evaluation) !== sequence) return undefined
  if (!fold.isCurrent(conclusion.evaluation) || !fold.running) return undefined
  switch (conclusion.result) {
    case 'done': return { write: 'completed', text: conclusionResult(conclusion) }
    case 'blocked': return { write: 'blocked', text: conclusionResult(conclusion) }
    default: return { write: conclusion.result }
  }
}

/** Goal's continuation for a `continue` conclusion: C1, C5 and C6 (§4.1). */
export function decideEvaluatedContinuation(fold: Fold, limit: number): Decision {
  if (!fold.running || fold.pendingDriving) return undefined
  if (fold.count >= limit) return { write: 'pause', reason: 'limit' }
  return { write: 'continuation', count: fold.count + 1 }
}
