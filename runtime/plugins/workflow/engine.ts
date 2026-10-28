import { randomUUID } from 'node:crypto'
import { setImmediate as immediate, setTimeout as delay } from 'node:timers/promises'
import type { InputWrite, PermissionPolicy, SessionRef, Sessions } from '../../../npm/plugin.js'
import { Catalog, select } from './catalog.ts'
import { parameters, policy, render, stages, WorkflowError, type Source, type StageDefinition } from './definition.ts'
import { StageHistory } from './history.ts'
import { bounded, log, RunStore, stageTerminal, terminal, type Run, type Stage } from './store.ts'

export interface Caller { sessionId: string; branchId: string; callId: string; cwd: string }
export interface StartRequest {
  name: string; source?: Source; cwd: string; params: Record<string, string>
  caller?: Caller; report_to?: { session_id: string; branch_id: string }
}

const need = (runs: Run[], id: string, sessionId?: string): Run => {
  const run = runs.find(run => run.run_id === id)
  if (!run || (sessionId !== undefined && run.report_to?.session_id !== sessionId
    && (run.started_by === 'management' || run.started_by.session_id !== sessionId))) {
    throw new WorkflowError('run_not_found')
  }
  return run
}
const failed = (run: Run) => run.stages.some(stage => ['failed', 'stopped', 'lost'].includes(stage.state))
const escaped = (text: string) => text.replace(/[&<>"']/g, character => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&apos;' })[character]!)

/** One fixed process row owns runs, native operations and store writes. */
export class Workflow {
  readonly catalog: Catalog
  readonly sessions: Sessions
  readonly store: RunStore
  readonly jobs = new Map<string | symbol, Promise<unknown>>()
  readonly abort = new AbortController()
  closing = false
  closingAt: number | undefined
  fault: Error | undefined
  pruning: Promise<void> | undefined
  maintenance: ReturnType<typeof setInterval> | undefined

  constructor(catalog: Catalog, sessions: Sessions, store: RunStore) {
    this.catalog = catalog
    this.sessions = sessions
    this.store = store
  }

  available() {
    if (this.fault) throw this.fault
    if (this.closing) throw new WorkflowError('service_reloading')
  }

  private fail(error: unknown) {
    this.fault = error instanceof Error ? error : new Error(String(error))
    console.error('Workflow', this.fault.message)
  }

  track<T>(work: () => Promise<T>): Promise<T> {
    this.available()
    const id = Symbol('call')
    const task = Promise.resolve().then(work).finally(() => this.jobs.delete(id))
    this.jobs.set(id, task)
    return task
  }

  private own(id: string | symbol, work: () => Promise<void>) {
    const task = immediate().then(work).catch(error => this.fail(error)).finally(() => {
      this.jobs.delete(id)
      if (!this.closing) void this.retain().catch(error => console.error('Workflow', 'retention failed', error))
    })
    this.jobs.set(id, task)
  }

  async restore() {
    const pending = await this.store.update(runs => {
      for (const run of runs) {
        if (!terminal(run)) {
          for (const stage of run.stages) {
            if (stage.state === 'launching' || stage.state === 'running') {
              stage.state = 'lost'
              stage.reason = 'owner_interrupted'
            } else if (stage.state === 'waiting') stage.state = 'skipped'
          }
          this.finish(run, 'interrupted')
        }
      }
      return runs.filter(run => run.report.state === 'pending').map(run => run.run_id)
    })
    this.own(Symbol('startup'), async () => {
      for (const id of pending) this.own(id, () => this.report(id))
    })
    this.maintenance = setInterval(() => {
      if (!this.closing) void this.retain().catch(error => console.error('Workflow', 'retention failed', error))
    }, 60_000)
  }

  async start(request: StartRequest, cancelled: () => boolean) {
    this.available()
    const configured = await this.catalog.load(request.cwd)
    const definition = select(configured.entries, request.name, request.source)
    const accepted = parameters(definition, request.params)
    const source = configured.entries.find(entry => entry.enabled && entry.definition === definition)!.source
    const run: Run = {
      run_id: randomUUID(), name: definition.name, source, definition, params: accepted, cwd: request.cwd,
      ...(request.caller ? { report_to: { session_id: request.caller.sessionId, branch_id: request.caller.branchId } }
        : request.report_to ? { report_to: request.report_to } : {}),
      started_by: request.caller ? { kind: 'tool', session_id: request.caller.sessionId,
        branch_id: request.caller.branchId, call_id: request.caller.callId } : 'management',
      created_at: new Date().toISOString(), state: 'running', report: { state: 'none' }, log: [],
      stages: definition.steps.flatMap((step, index) => stages(step).map(stage => ({ id: stage.id, step: index, state: 'waiting' as const }))),
    }
    await this.store.update(runs => {
      this.available()
      if (cancelled()) throw new WorkflowError('cancelled')
      if (runs.filter(run => !terminal(run)).length >= configured.document.max_concurrent_runs) {
        throw new WorkflowError('limit_exceeded')
      }
      log(run, 'Run accepted')
      runs.push(run)
    })
    this.own(run.run_id, () => this.drive(run.run_id, configured.document.max_concurrent_stages))
    return run
  }

  status(id: string, sessionId?: string): Promise<Run>
  status(id?: string, sessionId?: string): Promise<Run | Run[]>
  async status(id?: string, sessionId?: string) {
    this.available()
    const runs = await this.store.read()
    return id === undefined ? runs : need(runs, id, sessionId)
  }

  async stop(id: string, sessionId?: string) {
    this.available()
    const active = await this.store.update(runs => {
      const run = need(runs, id, sessionId)
      if (terminal(run)) return false
      run.state = 'stopping'
      log(run, 'Stop requested')
      for (const stage of run.stages) if (stage.state === 'waiting') stage.state = 'skipped'
      return true
    })
    if (active) await this.jobs.get(id)
    return this.status(id, sessionId)
  }

  private finish(run: Run, state: 'completed' | 'failed' | 'stopped' | 'interrupted') {
    run.state = state
    run.ended_at = new Date().toISOString()
    run.report = run.report_to && state !== 'stopped'
      ? { state: 'pending', submission_id: `workflow:${randomUUID()}`, attempt_open: false } : { state: 'none' }
    log(run, `Run ${state}`)
  }

  private async current(id: string) { return need(await this.store.read(), id) }

  private async drive(id: string, limit: number) {
    for (let step = 0;; step++) {
      const run = await this.current(id)
      if (this.fault) return
      if (this.closing || run.state === 'stopping' || failed(run) || step === run.definition.steps.length) break
      const definitions = stages(run.definition.steps[step])
      const results: Record<string, string> = Object.fromEntries(run.stages
        .filter(stage => stage.state === 'completed').map(stage => [stage.id, stage.result_text!]))
      const prompts = new Map<string, string>()
      const oversized = new Set<string>()
      for (const stage of definitions) {
        try { prompts.set(stage.id, render(stage, run.params, results)) }
        catch (error) {
          if (!(error instanceof WorkflowError) || error.code !== 'prompt_too_large') throw error
          oversized.add(stage.id)
        }
      }
      if (oversized.size) {
        await this.store.update(runs => {
          const run = need(runs, id)
          for (const stage of run.stages.filter(stage => stage.step === step)) {
            stage.state = oversized.has(stage.id) ? 'failed' : 'skipped'
            if (oversized.has(stage.id)) stage.reason = 'prompt_too_large'
          }
        })
        break
      }
      // Capture and persist A17 authority before admitting any sibling.
      await this.admit(id, definitions[0])
      let next = 0
      await Promise.all(Array.from({ length: Math.min(limit, definitions.length) }, async () => {
        try {
          while (next < definitions.length) {
            const index = next++
            const stage = definitions[index]
            if (index !== 0) await this.admit(id, stage)
            await this.executeStage(id, stage.id, prompts.get(stage.id)!)
          }
        } catch (error) {
          this.fail(error)
        }
      }))
    }
    if (this.fault) return
    await this.store.update(runs => {
      const run = need(runs, id)
      for (const stage of run.stages) if (stage.state === 'waiting') stage.state = 'skipped'
      this.finish(run, this.closing ? 'interrupted' : run.state === 'stopping' ? 'stopped' : failed(run) ? 'failed' : 'completed')
    })
    if (!this.closing) await this.report(id)
  }

  private async admit(id: string, definition: StageDefinition) {
    const run = await this.store.update(runs => {
      const run = need(runs, id)
      const stage = run.stages.find(stage => stage.id === definition.id)!
      if (this.closing || this.fault || run.state !== 'running' || failed(run)) {
        if (stage.state === 'waiting') stage.state = 'skipped'
        return undefined
      }
      stage.state = 'launching'
      if (definition.agent !== undefined) stage.agent_id = definition.agent
      return run
    })
    if (!run) return
    const starter: SessionRef | undefined = !run.policy_cap && run.started_by !== 'management'
      ? { sessionId: run.started_by.session_id, branchId: run.started_by.branch_id } : undefined
    const requested: PermissionPolicy | undefined = run.policy_cap ?? policy(run.definition)
    const reply = await this.sessions.launch({ cwd: run.cwd,
      ...(starter ? { starter } : {}), ...(requested ? { policy: requested } : {}),
      ...(definition.agent === undefined ? {} : { agent: definition.agent }),
      ...(definition.model === undefined ? {} : { model: definition.model }),
    })
    await this.store.update(runs => {
      const run = need(runs, id)
      const stage = run.stages.find(stage => stage.id === definition.id)!
      if (reply.ok) {
        stage.session_id = reply.value.sessionId
        if (!run.policy_cap) run.policy_cap = reply.value.policy
      } else { stage.state = 'failed'; stage.reason = reply.error.code }
    })
  }

  private async executeStage(id: string, stageId: string, prompt: string) {
    let run = await this.current(id)
    let stage = run.stages.find(stage => stage.id === stageId)!
    if (stageTerminal(stage)) return
    if (this.closing || run.state === 'stopping' || failed(run)) {
      await this.store.update(runs => { need(runs, id).stages.find(stage => stage.id === stageId)!.state = 'stopped' })
      return
    }
    if (!stage.session_id) throw new WorkflowError('invalid_data', 'Launch reply was not persisted')
    const sessionId = stage.session_id
    const submissionId = `workflow:${randomUUID()}`
    const accepted = await this.store.update(runs => {
      const run = need(runs, id)
      const stage = run.stages.find(stage => stage.id === stageId)!
      if (this.closing || this.fault || run.state !== 'running' || failed(run)) { stage.state = 'stopped'; return false }
      stage.submission_id = submissionId
      return true
    })
    if (!accepted) return
    const input: InputWrite = { sessionId, branchId: 'main', mode: 'queue', submissionId,
      rawBlocks: [{ text: `<dim-workflow kind="stage" run="${escaped(id)}" stage="${escaped(stageId)}"></dim-workflow>` }],
      content: [{ type: 'text', text: prompt }],
    }
    let receipt = await this.sessions.input(input)
    let inputId: string | undefined
    while (inputId === undefined) {
      if (receipt.ok) { inputId = receipt.value.inputId; break }
      const code = receipt.error.code
      if (!['reloading', 'publication_unknown'].includes(code)) {
        await this.store.update(runs => {
          const stage = need(runs, id).stages.find(stage => stage.id === stageId)!
          stage.state = 'failed'; stage.reason = code
        })
        return
      }
      for (;;) {
        if (this.fault) return
        const lookup = await this.sessions.lookup({ sessionId, branchId: 'main', submissionId })
        if (lookup.ok && lookup.value.status === 'accepted') { inputId = lookup.value.inputId; break }
        if (!lookup.ok && lookup.error.code !== 'reloading') {
          await this.store.update(runs => {
            const stage = need(runs, id).stages.find(stage => stage.id === stageId)!
            stage.state = 'failed'; stage.reason = lookup.error.code
          })
          return
        }
        if (lookup.ok && lookup.value.status === 'unknown') {
          const run = await this.current(id)
          if (this.closing || run.state === 'stopping' || failed(run)) {
            await this.store.update(runs => { need(runs, id).stages.find(stage => stage.id === stageId)!.state = 'stopped' })
            return
          }
          await this.pause()
          receipt = await this.sessions.input(input)
          break
        }
        if (this.closing && Date.now() - this.closingAt! >= 10_000) {
          await this.store.update(runs => {
            const stage = need(runs, id).stages.find(stage => stage.id === stageId)!
            stage.state = 'lost'; stage.reason = 'owner_interrupted'
          })
          return
        }
        await this.pause()
      }
    }
    await this.store.update(runs => {
      const stage = need(runs, id).stages.find(stage => stage.id === stageId)!
      stage.input_event_id = inputId
      stage.state = 'running'
    })
    const history = new StageHistory(this.sessions, sessionId, inputId)
    let stopSent = false
    for (;;) {
      if (this.fault) return
      const complete = await history.read()
      if (complete && history.outcome) {
        await this.store.update(runs => {
          Object.assign(need(runs, id).stages.find(stage => stage.id === stageId)!, history.outcome)
        })
        return
      }
      run = await this.current(id)
      if (this.closing && Date.now() - this.closingAt! >= 10_000) {
        await this.store.update(runs => {
          const stage = need(runs, id).stages.find(stage => stage.id === stageId)!
          stage.state = 'lost'; stage.reason = 'owner_interrupted'
        })
        return
      }
      if (complete && (this.closing || run.state === 'stopping' || failed(run))) {
        if (!history.consumed) {
          const withdrawn = await this.sessions.withdraw({ sessionId, branchId: 'main', inputId, reason: 'workflow stopping' })
          if (withdrawn.ok && withdrawn.value.status === 'deleted') {
            await this.store.update(runs => { need(runs, id).stages.find(stage => stage.id === stageId)!.state = 'stopped' })
            return
          }
          if (!withdrawn.ok && withdrawn.error.code !== 'reloading') {
            await this.store.update(runs => {
              const stage = need(runs, id).stages.find(stage => stage.id === stageId)!
              stage.state = 'failed'; stage.reason = withdrawn.error.code
            })
            return
          }
        } else if (!stopSent) {
          const reply = await this.sessions.stop({ sessionId, branchId: 'main', reason: 'workflow stopping' })
          if (reply.ok) stopSent = true
          else if (!['reloading', 'session_closed', 'branch_missing'].includes(reply.error.code)) {
            await this.store.update(runs => {
              const stage = need(runs, id).stages.find(stage => stage.id === stageId)!
              stage.state = 'failed'; stage.reason = reply.error.code
            })
            return
          }
        }
      }
      await this.pause()
    }
  }

  private async pause() {
    try { await delay(1000, undefined, this.closing ? undefined : { signal: this.abort.signal }) }
    catch (error) { if (!this.closing) throw error }
  }

  private reportText(run: Run): string {
    const last = run.definition.steps.length - 1
    const preview = bounded(run.stages.filter(stage => stage.step === last)
      .map(stage => `${stage.id}: ${stage.result_text ?? stage.reason ?? stage.state}`).join('\n'), 4 * 1024)
    const text = `Workflow ${run.run_id}: ${run.state}.\n${run.stages.map(stage => `${stage.id}: ${stage.state}`).join('\n')}\n${preview}\nUse workflow status with run_id ${run.run_id} for stored stage results.`
    return `<dim-workflow kind="report" run="${escaped(run.run_id)}" status="${run.state}">${escaped(text)}</dim-workflow>`
  }

  /** Sends the pending report. A retry, including one by a replacement
   * generation, resubmits the same submission id: the destination branch
   * accepts it once and replies the first input's receipt. */
  private async report(id: string) {
    while (!this.closing && !this.fault) {
      const run = await this.current(id)
      if (run.report.state !== 'pending') return
      const report = run.report
      const address = { sessionId: run.report_to!.session_id, branchId: run.report_to!.branch_id }
      await this.store.update(runs => {
        const pending = need(runs, id).report
        if (pending.state === 'pending') pending.attempt_open = true
      })
      const text = this.reportText(run)
      const reply = await this.sessions.input({ ...address, submissionId: report.submission_id, mode: 'steer',
        rawBlocks: [{ text }], content: [],
      })
      await this.store.update(runs => {
        const run = need(runs, id)
        if (reply.ok) run.report = { state: 'delivered' }
        else if (reply.error.code === 'ambiguous') {
          run.report = { state: 'delivered' }
          log(run, 'Duplicate report violation: input ambiguous')
        } else if (['session_closed', 'branch_missing', 'branch_ended', 'rejected'].includes(reply.error.code)) {
          run.report = { state: 'undeliverable', reason: reply.error.code }
          if (reply.error.code === 'rejected') log(run, 'Invalid report rejected')
        } else {
          const pending = run.report
          if (pending.state === 'pending') pending.attempt_open = false
          log(run, `Report input failed: ${reply.error.code}`)
        }
      })
      if (reply.ok || !['publication_unknown', 'reloading'].includes(reply.error.code)) return
      await this.pause()
    }
  }

  private retain(): Promise<void> {
    if (this.pruning) return this.pruning
    const task = this.prune().finally(() => { this.pruning = undefined })
    this.pruning = task
    return task
  }

  private async prune() {
    if (this.closing) return
    const runs = await this.store.read()
    const terminalRuns = runs.filter(run => terminal(run) && run.report.state !== 'pending')
      .sort((a, b) => b.ended_at!.localeCompare(a.ended_at!) || b.run_id.localeCompare(a.run_id))
    const expired = terminalRuns.slice(100).filter(run => !this.jobs.has(run.run_id))
    const expiring = new Set(expired.map(run => run.run_id))
    const references = new Set(runs.filter(run => !expiring.has(run.run_id))
      .flatMap(run => run.stages.flatMap(stage => stage.session_id ? [stage.session_id] : [])))
    for (const run of expired) {
      if (this.closing) return
      let removed = true
      for (const sessionId of new Set(run.stages.flatMap(stage => stage.session_id ? [stage.session_id] : []))) {
        if (references.has(sessionId)) continue
        const reply = await this.sessions.delete({ sessionId })
        if (!reply.ok && reply.error.code !== 'session_closed') {
          removed = false
          console.error('Workflow', 'retention delete failed', reply.error.code)
          await this.store.update(runs => { log(need(runs, run.run_id), `Retention delete failed: ${reply.error.code}`) })
          break
        }
      }
      if (removed) await this.store.update(runs => {
        const index = runs.findIndex(candidate => candidate.run_id === run.run_id)
        if (index >= 0) runs.splice(index, 1)
      })
    }
  }

  async close() {
    this.closing = true
    this.closingAt = Date.now()
    clearInterval(this.maintenance)
    this.abort.abort()
    try {
      while (this.jobs.size) await Promise.allSettled([...this.jobs.values()])
      await this.pruning
    } finally {
      await this.store.scope.close()
    }
  }
}
