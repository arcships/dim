import { definePlugin } from '@arcships/rutis'
import type { SettingsResult } from '../../../npm/plugin.d.ts'
import {
  type AgentEvent, type Conclusion, type InputMessage, type Texts, Fold, OBJECTIVE_KEY, STATUS_KEY, conclusionOf,
  decideConclusion, decideEvaluatedContinuation, decideEvaluation, decideInterrupt, decideTurnEnd,
  evaluationOfOperation, evaluationOperation, evaluatorPrompt, isOpening, marker, replyConclusion,
  truncate, writes,
} from './goal.ts'

type TurnEnd = { turn_id: string; status: string }
type Dispatcher = {
  consume(
    name: string,
    keys: string[],
    rebuild: () => number | bigint | Promise<number | bigint>,
    handler: (event: AgentEvent) => void | Promise<void>,
  ): Promise<void>
}
type TimelineQuery = {
  scope: 'branch' | 'chain'
  include_publishing?: boolean
  from_event?: string
  from_sequence?: number | bigint
  order_by?: 'sequence'
  direction?: 'ascending' | 'descending'
  kinds?: string[]
  flags?: ('render' | 'state')[]
  event_id?: string
  predicate?: { path: string; value: unknown }
  limit: number | bigint
  offset?: number | bigint
}
type TimelineReply = { events: AgentEvent[]; positions: { branch_id: string; sequence: number | bigint }[] }
type Timeline = {
  query(query: TimelineQuery): Promise<TimelineReply & { through: number | bigint }>
}
type BranchOptions = { prompt: string; source: { kind: 'host'; operation_id: string } }
type TimelineInfo = {
  id: string
  parent?: string | null
  source?: { kind: string; operation_id?: string } | null
}
type TreeCommands = {
  input(id: string, input: InputMessage): Promise<AgentEvent>
  create(parent: string, at: number | bigint, options: BranchOptions, cancelled: () => void | Promise<void>): Promise<string>
  end(id: string, source: { kind: 'host'; operation_id: string }): Promise<void>
  info(id: string): Promise<TimelineInfo>
}
type Settings = {
  read(request: { source: string; scope: 'user' }): Promise<SettingsResult<{ document: { maxContinuations: number } }>>
}

const KEYS = ['branch/start', 'session/forked', 'branch/create', 'branch/create-failed', 'branch/ended', 'input/message', 'input/consumed', 'input/deleted', 'turn/start', 'turn/end', 'stop/interrupt']

/** Inherited `input/message` facts one page of the new-branch check keeps. */
const INHERITED_PAGE = 64n

/** Facts one page of the rebuild reads. */
const REBUILD_PAGE = 256n

/** Facts one page of the reply read keeps. */
const REPLY_PAGE = 256n

const codeOf = (error: unknown): string =>
  typeof error === 'object' && error !== null && 'code' in error
    ? String((error as { code: unknown }).code)
    : String(error)

// GoalBranchPlugin (Goal.md §1, A.6): owns the branch's fold, decides
// continuation from consumed state only, and writes through owned tasks. On a
// branch Goal created for an evaluation it takes the evaluator role instead
// (Pursuit.md §4.2).
export default definePlugin({
  name: 'GoalBranch',
  provides: {},
  inject: ['dispatcher', 'timeline', 'timelineIdentity', 'treeCommands', 'settings', 'branchLifecycle', 'sessionLifecycle'],
  async apply(ctx, texts: Texts) {
    const dispatcher = ctx.use<Dispatcher>('dispatcher')
    const timeline = ctx.use<Timeline>('timeline')
    const tree = ctx.use<TreeCommands>('treeCommands')
    const settings = ctx.use<Settings>('settings')
    const { branchId } = await ctx.use<{ get(): Promise<{ branchId: string }> }>('timelineIdentity').get()
    const lifecycle = ctx.use<{ activated(): Promise<boolean>; closing(): Promise<void> }>('branchLifecycle')
    const session = ctx.use<{ activated(): Promise<boolean> }>('sessionLifecycle')
    let cancel = () => {}
    const cancellation = new Promise<void>(resolve => { cancel = resolve })
    let stopped = false
    ctx.effect(() => { stopped = true; cancel() })
    void lifecycle.closing().then(() => { stopped = true; cancel() })

    const fold = new Fold()
    let start: bigint | undefined
    let role: { branch: string; evaluation: string } | undefined
    const resolved = Promise.withResolvers<void>()
    void (async () => {
      try {
        if (!(await lifecycle.activated()) || stopped) return
        const info = await tree.info(branchId)
        const parent = typeof info.parent === 'string' ? info.parent : undefined
        const evaluation = info.source?.kind === 'host' ? evaluationOfOperation(info.source.operation_id) : undefined
        if (parent !== undefined && evaluation !== undefined) role = { branch: parent, evaluation }
      } catch {
      } finally {
        resolved.resolve()
      }
    })()
    const inFlight = new Set<string>()
    const task = (key: string, work: () => Promise<void>) => {
      if (stopped || inFlight.has(key)) return
      inFlight.add(key)
      void (async () => {
        try {
          if (!(await lifecycle.activated()) || stopped) return
          await work()
        } catch (error) {
          console.warn(`Goal write failed (${key}):`, codeOf(error))
        } finally {
          inFlight.delete(key)
        }
      })()
    }
    const interrupted = (at: bigint) => task(`interrupt:${at}`, async () => {
      if (decideInterrupt(fold, at)) await tree.input(branchId, writes.pause(branchId, 'interrupted', at))
    })
    // Whether this branch already committed its `branch/create` for `after`:
    // from then on the creation records its own outcome, so a throw only stops
    // the caller waiting (Pursuit §4.1).
    const created = async (after: bigint) => {
      const operation = evaluationOperation(String(after))
      // The creation may still be in flight when its wait was abandoned, so the
      // read adds the facts written and not yet published.
      const page = await timeline.query({
        scope: 'branch',
        include_publishing: true,
        from_sequence: after,
        order_by: 'sequence',
        direction: 'ascending',
        kinds: ['branch/create'],
        predicate: { path: 'source.operation_id', value: operation },
        limit: 1n,
        offset: 0n,
      })
      return page.events.length > 0
    }
    const doTurnEnd = async (after: bigint) => {
      if (fold.turnEnd?.sequence !== after) return
      const request = fold.requestInTurn(after)
      if (request && decideEvaluation(fold, after)) {
        const evaluation = String(after)
        try {
          await tree.create(branchId, after, {
            prompt: evaluatorPrompt(branchId, evaluation, fold.objective ?? '', request.summary),
            source: { kind: 'host', operation_id: evaluationOperation(evaluation) },
          }, () => cancellation)
        } catch {
          if (!stopped && !(await created(after)) && fold.isCurrent(String(after))) {
            await tree.input(branchId, writes.pause(branchId, 'evaluation_failed', after))
          }
        }
        settle()
        return
      }
      const read = await settings.read({ source: 'goal', scope: 'user' })
      if (stopped || fold.turnEnd?.sequence !== after) return
      if (!read.ok) {
        console.warn('Goal settings read failed:', read.error.code)
        return
      }
      const limit = read.value.document.maxContinuations
      const decision = decideTurnEnd(fold, limit)
      if (!decision) return
      await tree.input(branchId, decision.write === 'continuation'
        ? writes.continuation(branchId, decision.count, limit, after, texts)
        : writes.pause(branchId, decision.reason, after))
    }
    const evaluationFailed = (evaluation: string) => task(`failed:${evaluation}`, async () => {
      if (!fold.isCurrent(evaluation)) return
      await tree.input(branchId, writes.pause(branchId, 'evaluation_failed', BigInt(evaluation)))
    })
    // The conclusion the evaluator wrote to this branch decides the evaluation
    // it names (Pursuit §4.1).
    const concluded = (sequence: bigint, conclusion: Conclusion) => task(`conclusion:${sequence}`, async () => {
      const action = decideConclusion(fold, sequence, conclusion)
      if (!action) return
      const evaluation = BigInt(conclusion.evaluation)
      if (action.write === 'failed') {
        await tree.input(branchId, writes.pause(branchId, 'evaluation_failed', evaluation))
        return
      }
      if (action.write === 'completed' || action.write === 'blocked') {
        await tree.input(branchId, writes.evaluated(branchId, action.write, conclusion.evaluation, action.text))
        return
      }
      const read = await settings.read({ source: 'goal', scope: 'user' })
      if (stopped) return
      if (!read.ok) {
        console.warn('Goal settings read failed:', read.error.code)
        return
      }
      if (decideConclusion(fold, sequence, conclusion)?.write !== 'continue') return
      const limit = read.value.document.maxContinuations
      const decision = decideEvaluatedContinuation(fold, limit)
      if (!decision) return
      await tree.input(branchId, decision.write === 'continuation'
        ? writes.evaluatedContinuation(branchId, decision.count, limit, evaluation, conclusion.evaluation)
        : writes.pause(branchId, decision.reason, evaluation))
    })
    // The reply is the turn's final model request: the text deltas of the
    // request its last `step/model-start` names (Pursuit §4.2). The turn id is
    // one equality, so the store selects the turn's facts, and the turn's end
    // is where the read stops.
    const reply = async (turnStart: bigint, through: bigint, turnId: string) => {
      const events: AgentEvent[] = []
      let from = turnStart
      for (;;) {
        const page = await timeline.query({
          scope: 'branch',
          from_sequence: from,
          order_by: 'sequence',
          direction: 'ascending',
          kinds: ['step/model-start', 'step/text'],
          predicate: { path: 'turn_id', value: turnId },
          limit: REPLY_PAGE,
          offset: 0n,
        })
        events.push(...page.events.filter(event => BigInt(event.sequence) <= through))
        const last = page.events.at(-1)
        if (last === undefined || BigInt(page.events.length) < REPLY_PAGE || BigInt(last.sequence) >= through) break
        from = BigInt(last.sequence) + 1n
      }
      const starts = events.filter(event => event.type === 'step' && event.name === 'model-start' && event.data.turn_id === turnId)
      const request = starts.at(-1)?.data.request_id
      if (request === undefined) return ''
      return events
        .filter(event => event.type === 'step' && event.name === 'text' && event.data.turn_id === turnId && event.data.request_id === request)
        .map(event => event.data.delta as string)
        .join('')
    }
    const evaluated = (sequence: bigint, turnStart: bigint, end: TurnEnd) =>
      task(`evaluate:${sequence}`, async () => {
        if (!role || fold.turnEnd?.sequence !== sequence) return
        const report = (result: 'done' | 'continue' | 'blocked' | 'failed', text: string) =>
          tree.input(role.branch, writes.conclusion(role.branch, branchId, role.evaluation, result, text))
        if (end.status !== 'completed') {
          await report('failed', "the evaluator's turn did not complete")
          return
        }
        const text = await reply(turnStart, sequence, end.turn_id)
        const conclusion = replyConclusion(text)
        if (conclusion) {
          await report(conclusion.result, conclusion.text)
        } else if (fold.turnEnds === 1) {
          await tree.input(branchId, writes.evaluateAgain(branchId, sequence))
        } else {
          await report('continue', truncate(text))
        }
      })
    // The inherited prefix carries the parent's goal blocks; the branch's own
    // events are the fold's, not the prefix's. Newest first, so the first
    // event with a keyed block decides and the rest is not read.
    const inheritedLive = async (at: bigint) => {
      let offset = 0n
      for (;;) {
        const page = await timeline.query({
          scope: 'chain',
          from_sequence: at,
          order_by: 'sequence',
          direction: 'descending',
          kinds: ['input/message'],
          limit: INHERITED_PAGE,
          offset,
        })
        for (const [index, event] of page.events.entries()) {
          if (page.positions[index].branch_id === branchId) continue
          let live: boolean | undefined
          for (const raw of event.data.blocks ?? []) {
            if (marker(raw.text) && (raw.key === OBJECTIVE_KEY || raw.key === STATUS_KEY)) live = !raw.retire
          }
          if (live !== undefined) return live
        }
        if (page.events.length < Number(INHERITED_PAGE)) return false
        offset += BigInt(page.events.length)
      }
    }
    const decideStart = (at: bigint) => task(`clear:${at}`, async () => {
      if (fold.isDecided(at) || fold.localBlocks > 0) return
      const live = fold.copiedLive ?? await inheritedLive(at)
      if (!stopped && live && !fold.isDecided(at) && fold.localBlocks === 0) {
        await tree.input(branchId, writes.startClear(branchId, at))
      }
    })
    const decideEnd = () => {
      const end = fold.turnEnd
      if (!end || !fold.running || fold.isDecided(end.sequence)) return
      const sequence = end.sequence
      task(`turn:${sequence}`, async () => {
        await resolved.promise
        if (stopped || role || fold.turnEnd?.sequence !== sequence) return
        await doTurnEnd(sequence)
      })
    }
    const turnDecision = (sequence: bigint, turnStart: bigint, end: TurnEnd) =>
      task(`turn:${sequence}`, async () => {
        await resolved.promise
        if (stopped) return
        if (role) return evaluated(sequence, turnStart, end)
        if (fold.running && !fold.isDecided(sequence)) await doTurnEnd(sequence)
      })
    const decided = (evaluation: string) =>
      fold.conclusions.has(evaluation) || !fold.isCurrent(evaluation)
    // Pursuit §13 E2.
    const settle = () => {
      for (const [evaluation, evaluator] of fold.evaluations) {
        if (fold.ended.has(evaluator) || fold.failed.has(evaluator) || !decided(evaluation)) continue
        task(`settle:${evaluation}`, async () => {
          await resolved.promise
          if (!(await session.activated())) return
          if (stopped || fold.ended.has(evaluator) || !decided(evaluation)) return
          try {
            await tree.end(evaluator, { kind: 'host', operation_id: evaluationOperation(evaluation) })
          } catch (error) {
            if (codeOf(error) !== 'BranchEnded') throw error
          }
        })
      }
    }
    const ingest = (event: AgentEvent, live: boolean) => {
      const turnStart = fold.turnStart
      fold.apply(event)
      const sequence = BigInt(event.sequence)
      if (isOpening(event)) {
        start = sequence
        if (live) decideStart(sequence)
        return
      }
      if (!live) return
      if (event.type === 'branch' && event.name === 'create-failed') {
        const evaluation = fold.evaluationOf(event.data.branch)
        if (evaluation !== undefined) evaluationFailed(evaluation)
      } else if (event.type === 'branch' && event.name === 'create') {
        settle()
      } else if (event.type === 'branch' && event.name === 'ended') {
        const evaluation = fold.evaluationOf(event.data.branch)
        if (evaluation !== undefined && fold.isCurrent(evaluation) && !fold.conclusions.has(evaluation)) {
          evaluationFailed(evaluation)
        }
      } else if (event.type === 'turn' && event.name === 'end') {
        turnDecision(sequence, turnStart, event.data as TurnEnd)
      } else if (event.type === 'stop' && event.name === 'interrupt' && event.data.scope === 'turn') {
        interrupted(sequence)
      } else if (event.type === 'input' && event.name === 'message') {
        const first = event.data.blocks?.[0]
        const conclusion = first && conclusionOf(first.text)
        if (conclusion) concluded(sequence, conclusion)
        settle()
      } else if (event.type === 'input' && event.name === 'deleted') {
        decideEnd()
      }
    }

    // Fold this branch's facts through the cutoff of one read, unpublished
    // tail included, and release only what lies above it (A.6 Load).
    await dispatcher.consume('Goal', KEYS, async () => {
      let from: bigint | undefined
      let through = 0n
      for (;;) {
        const reply = await timeline.query({
          scope: 'branch',
          include_publishing: true,
          ...(from === undefined ? {} : { from_sequence: from }),
          order_by: 'sequence',
          direction: 'ascending',
          limit: REBUILD_PAGE,
          offset: 0n,
        })
        for (const event of reply.events) ingest(event, false)
        const last = reply.events.at(-1)
        if (last === undefined || BigInt(reply.events.length) < REBUILD_PAGE) {
          through = BigInt(reply.through)
          break
        }
        from = BigInt(last.sequence) + 1n
      }
      // The decisions the previous generation owed are taken once, before any
      // live event.
      decideEnd()
      settle()
      if (start !== undefined) decideStart(start)
      return through
    }, event => ingest(event, true))
  },
})
