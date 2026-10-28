import { definePlugin } from '@arcships/rutis'
import type { SettingsResult } from '../../../npm/plugin.d.ts'
import {
  type AgentEvent, type Conclusion, type InputMessage, Fold, OBJECTIVE_KEY, STATUS_KEY, conclusionOf,
  decideConclusion, decideEvaluatedContinuation, decideEvaluation, decideInterrupt, decideTurnEnd,
  evaluationOperation, evaluatorPrompt, isOpening, marker, replyConclusion, truncate, writes,
} from './goal.ts'

type Delivery = { replay: AgentEvent[] } | { live: AgentEvent }
type TurnEnd = { turn_id: string; status: string }
type Dispatcher = {
  consume(name: string, keys: string[], handler: (delivery: Delivery) => void | Promise<void>): Promise<void>
  local(start: number | bigint, through: number | bigint): Promise<AgentEvent[]>
  history(through: number | bigint): Promise<AgentEvent[]>
  committed(): Promise<number | bigint>
}
type BranchOptions = { prompt: string; source: { kind: 'host'; operation_id: string } }
type TreeCommands = {
  input(id: string, input: InputMessage): Promise<AgentEvent>
  create(parent: string, at: number | bigint, options: BranchOptions, cancelled: () => void | Promise<void>): Promise<string>
}
type Settings = {
  read(request: { source: string; scope: 'user' }): Promise<SettingsResult<{ document: { maxContinuations: number } }>>
}

const KEYS = ['branch/start', 'session/forked', 'branch/create', 'branch/create-failed', 'input/message', 'input/consumed', 'input/deleted', 'turn/start', 'turn/end', 'stop/interrupt']

// GoalBranchPlugin (Goal.md §1, A.6): owns the branch's fold, decides
// continuation from consumed state only, and writes through owned tasks. On a
// branch Goal created for an evaluation it takes the evaluator role instead
// (Pursuit.md §4.2).
export default definePlugin({
  name: 'GoalBranch',
  provides: {},
  inject: ['dispatcher', 'timelineIdentity', 'treeCommands', 'settings', 'branchLifecycle'],
  async apply(ctx) {
    const dispatcher = ctx.use<Dispatcher>('dispatcher')
    const tree = ctx.use<TreeCommands>('treeCommands')
    const settings = ctx.use<Settings>('settings')
    const { branchId } = await ctx.use<{ get(): Promise<{ branchId: string }> }>('timelineIdentity').get()
    const lifecycle = ctx.use<{ activated(): Promise<boolean>; closing(): Promise<void> }>('branchLifecycle')
    let cancel = () => {}
    const cancellation = new Promise<void>(resolve => { cancel = resolve })
    let stopped = false
    ctx.effect(() => { stopped = true; cancel() })
    void lifecycle.closing().then(() => { stopped = true; cancel() })

    const fold = new Fold()
    let start: bigint | undefined
    const inFlight = new Set<string>()
    const task = (key: string, work: () => Promise<void>) => {
      if (stopped || inFlight.has(key)) return
      inFlight.add(key)
      void (async () => {
        try {
          if (!(await lifecycle.activated()) || stopped) return
          await work()
        } finally {
          inFlight.delete(key)
        }
      })()
    }
    const startedClear = (at: bigint) => task(`clear:${at}`, async () => {
      if (!fold.isDecided(at) && fold.localBlocks === 0) await tree.input(branchId, writes.startClear(branchId, at))
    })
    const interrupted = (at: bigint) => task(`interrupt:${at}`, async () => {
      if (decideInterrupt(fold, at)) await tree.input(branchId, writes.pause(branchId, 'interrupted', at))
    })
    // Whether this branch already committed its `branch/create` for `after`:
    // from then on the creation records its own outcome, so a throw only stops
    // the caller waiting (Pursuit §4.1).
    const created = async (after: bigint) => {
      const operation = evaluationOperation(String(after))
      const through = await dispatcher.committed()
      return (await dispatcher.local(after, through)).some(event => event.type === 'branch'
        && event.name === 'create'
        && event.data.source?.kind === 'host'
        && event.data.source.operation_id === operation)
    }
    const turnEnd = (after: bigint) => task(`end:${after}`, async () => {
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
        ? writes.continuation(branchId, decision.count, limit, after)
        : writes.pause(branchId, decision.reason, after))
    })
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
    // request its last `step/model-start` names (Pursuit §4.2).
    const reply = async (turnStart: bigint, through: bigint, turnId: string) => {
      const events = await dispatcher.local(turnStart, through)
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
        const role = fold.evaluate
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
    const inheritedLive = async (at: bigint) => {
      const local = new Set((await dispatcher.local(at, at)).map(event => event.id))
      let live = false
      for (const event of await dispatcher.history(at)) {
        if (local.has(event.id) || event.type !== 'input' || event.name !== 'message') continue
        for (const raw of event.data.blocks ?? []) {
          if (marker(raw.text) && (raw.key === OBJECTIVE_KEY || raw.key === STATUS_KEY)) live = !raw.retire
        }
      }
      return live
    }
    const decideStart = (at: bigint) => {
      if (fold.isDecided(at) || fold.localBlocks > 0) return
      const live = fold.copiedLive !== undefined ? Promise.resolve(fold.copiedLive) : inheritedLive(at)
      void live.then(live => {
        if (live) startedClear(at)
      })
    }
    const decideEnd = () => {
      const end = fold.turnEnd
      if (end && fold.running && !fold.isDecided(end.sequence)) turnEnd(end.sequence)
    }
    const consume = (event: AgentEvent, live: boolean) => {
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
      } else if (fold.evaluate) {
        if (event.type === 'turn' && event.name === 'end') evaluated(sequence, turnStart, event.data as TurnEnd)
      } else if (event.type === 'turn' && event.name === 'end') {
        decideEnd()
      } else if (event.type === 'stop' && event.name === 'interrupt' && event.data.scope === 'turn') {
        interrupted(sequence)
      } else if (event.type === 'input' && event.name === 'message') {
        const first = event.data.blocks?.[0]
        const conclusion = first && conclusionOf(first.text)
        if (conclusion) concluded(sequence, conclusion)
      } else if (event.type === 'input' && event.name === 'deleted') {
        decideEnd()
      }
    }

    // Fold the replayed history; the decisions the previous generation owed
    // are taken once, before any live event (A.6 Load).
    await dispatcher.consume('Goal', KEYS, delivery => {
      if ('live' in delivery) return consume(delivery.live, true)
      for (const event of delivery.replay) consume(event, false)
      decideEnd()
      if (start !== undefined) decideStart(start)
    })
  },
})
