import { randomUUID } from 'node:crypto'
import type { Sessions } from '../../../npm/plugin.js'
import { effective, ScheduleError, within, type Job } from './data.ts'
import { RunHistory } from './history.ts'
import { SettingsReadError, type Jobs } from './jobs.ts'
import { unloadingBeforeCall } from './requests.ts'
import { terminal, type Run, type RunStore } from './store.ts'
import { latestWindow, windowAfter } from './windows.ts'

const pause = (milliseconds: number) => new Promise<void>(resolve => setTimeout(resolve, milliseconds))
const escaped = (text: string) => text.replace(/[&<>"']/g, character => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&apos;' })[character]!)

export class Scheduler {
  readonly jobs: Jobs
  readonly sessions: Sessions
  readonly store: RunStore
  closing = false
  fault: unknown
  readonly work = new Map<string, Promise<unknown>>()
  private pruning = false
  private retryPruneAt: number | undefined
  private nextWindowAt: number | undefined
  private rewake = false
  /// The report's guidance line: the factory configuration's text or
  /// the built-in one.
  private readonly reportGuidance: string

  constructor(jobs: Jobs, sessions: Sessions, store: RunStore, reportGuidance = 'Inspect schedule.history for this job.') {
    this.jobs = jobs; this.sessions = sessions; this.store = store; this.reportGuidance = reportGuidance
  }

  async track<T>(operation: () => Promise<T>, key: string = randomUUID()): Promise<T> {
    if (this.closing) throw new ScheduleError('service_reloading', unloadingBeforeCall)
    if (this.fault) throw this.fault
    const work = operation()
    this.work.set(key, work)
    try { return await work }
    finally { this.work.delete(key) }
  }

  async restore() {
    const runs = await this.store.read()
    for (const run of runs.filter(run => !terminal(run))) {
      this.background(run.run_id, () => this.resume(run))
    }
    this.requestPrune()
  }

  startTimer() {
    this.background('timer', async () => {
      let minute: number | undefined
      while (!this.closing && !this.fault) {
        const now = Date.now(), currentMinute = Math.floor(now / 60_000)
        if (minute !== currentMinute || this.nextWindowAt !== undefined && now >= this.nextWindowAt) {
          minute = currentMinute
          this.nextWindowAt = undefined
          this.changed()
        }
        if (this.retryPruneAt !== undefined && Date.now() >= this.retryPruneAt) this.requestPrune()
        await pause(1000)
      }
    })
  }

  changed() {
    if (this.closing || this.fault) return
    if (this.work.has('wake')) this.rewake = true
    else this.background('wake', async () => { this.nextWindowAt = await this.tick() })
  }

  private background(key: string, operation: () => Promise<unknown>) {
    void this.track(operation, key).catch(error => {
      if (key !== 'wake' && !(error instanceof SettingsReadError)) this.fault = error
      console.error('Scheduler', error instanceof Error ? error.message : String(error))
    }).finally(() => {
      if (key === 'wake' && this.rewake) {
        this.rewake = false
        this.changed()
      }
      if (!['timer', 'retention', 'wake'].includes(key) && !this.closing && !this.fault) {
        this.requestPrune()
        this.changed()
      }
    })
  }

  private requestPrune() {
    if (this.pruning) this.retryPruneAt ??= Date.now()
    else this.background('retention', () => this.prune())
  }

  async close() {
    this.closing = true
    while (this.work.size) await Promise.allSettled([...this.work.values()])
    await this.store.scope.close()
  }

  async tick() {
    const { document } = await this.jobs.read()
    const runs = await this.store.read()
    let nextWindow: number | undefined
    for (const job of document.jobs.filter(job => job.enabled)) {
      if (this.closing || this.fault) break
      try {
        const schedule = job.schedule
        if ('at' in schedule && runs.some(run => run.job_id === job.id && run.window === new Date(schedule.at).toISOString()
          && (terminal(run) || run.input_event_id))) {
          await this.jobs.settleWindow(job)
          continue
        }
        const windows = runs.filter(run => run.job_id === job.id && run.window !== 'manual').map(run => run.window)
        const previous = windows.length ? Date.parse(windows.sort().at(-1)!)
          : 'at' in schedule ? Number.NEGATIVE_INFINITY : Date.parse(job.enabled_at!) - 1
        const next = windowAfter(job, previous)
        if (next === undefined) continue
        if (next > Date.now()) {
          if (next < (Math.floor(Date.now() / 60_000) + 1) * 60_000) {
            nextWindow = Math.min(nextWindow ?? next, next)
          }
          continue
        }
        const window = Date.now() - next >= 60_000 ? latestWindow(job, Date.now()) : next
        await this.accept(job.id, new Date(window).toISOString(), Date.now() - window >= 60_000)
      } catch (error) {
        console.error('Scheduler job', job.id, error instanceof Error ? error.message : String(error))
      }
    }
    return nextWindow
  }

  async accept(jobId: string, window = 'manual', missed = false): Promise<Run> {
    const { document } = await this.jobs.read()
    const snapshot = document.jobs.find(job => job.id === jobId)
    if (!snapshot) throw new ScheduleError('schedule_not_found')
    const accepted = await this.store.update(runs => {
      const existing = window === 'manual' ? undefined : runs.find(run => run.job_id === jobId && run.window === window)
      if (existing) return { run: existing, fresh: false }
      const now = new Date(Date.now()).toISOString()
      const reason = missed ? 'missed' : !snapshot.enabled ? 'disabled'
        : runs.some(run => run.job_id === jobId && !terminal(run)) ? 'overlap'
        : runs.filter(run => !terminal(run)).length >= document.max_concurrent_runs ? 'capacity' : undefined
      const deadline = new Date(Date.now() + snapshot.max_run_minutes * 60_000)
      if (!reason && (!Number.isFinite(deadline.getTime()) || deadline.getUTCFullYear() > 9999)) {
        throw new ScheduleError('schedule_invalid', 'Run deadline exceeds the persisted timestamp range')
      }
      const runId = randomUUID()
      const run: Run = { run_id: runId, job_id: jobId, window, state: reason ? missed ? 'missed' : 'skipped' : 'launching',
        created_at: now, submission_id: `scheduler:${runId}`, detail: reason ?? '',
        ...(reason ? { ended_at: now } : {
          deadline_at: deadline.toISOString(),
          ...(snapshot.created_by === 'user' ? {} : { report_to: snapshot.created_by.tool }),
        }),
      }
      runs.push(run)
      return { run, fresh: true }
    })
    if (accepted.fresh && !terminal(accepted.run)) this.background(accepted.run.run_id, () => this.execute(snapshot, accepted.run))
    else if (accepted.fresh) {
      if (window !== 'manual' && 'at' in snapshot.schedule) await this.jobs.settleWindow(snapshot)
      this.requestPrune()
    }
    return accepted.run
  }

  private async check(snapshot: Job, runId: string, revision = snapshot.revision,
    binding = snapshot.session_id): Promise<Run | undefined> {
    let current: Job | undefined
    try { current = (await this.jobs.read()).document.jobs.find(job => job.id === snapshot.id) }
    catch (error) {
      if (!(error instanceof SettingsReadError)) throw error
      await this.finish(runId, 'skipped', 'settings_read_failed')
      return undefined
    }
    const run = (await this.store.read()).find(run => run.run_id === runId)!
    if (terminal(run) || this.closing || this.fault) return undefined
    if (Date.now() >= Date.parse(run.deadline_at!)) {
      await this.finish(runId, 'timed_out', 'deadline_before_input')
      return undefined
    }
    if (!current || current.revision !== revision || !current.enabled
      || current.mode !== snapshot.mode || current.session_id !== binding) {
      await this.finish(runId, 'skipped', current && !current.enabled ? 'disabled' : 'changed')
      return undefined
    }
    return run
  }

  private async execute(snapshot: Job, accepted: Run) {
    const submitted = await this.submit(snapshot, accepted)
    if (submitted) await this.follow({ ...accepted, session_id: submitted.sessionId, input_event_id: submitted.receipt.inputId }, submitted.receipt.sequence)
  }

  private async bind(snapshot: Job, runId: string, sessionId: string): Promise<number | 'unbound' | undefined> {
    for (let attempt = 0; attempt < 2; attempt++) {
      const run = (await this.store.read()).find(run => run.run_id === runId)!
      if (terminal(run) || this.closing || this.fault) return
      let result
      try {
        result = await this.jobs.bind(snapshot, sessionId)
      } catch (error) {
        if (error instanceof SettingsReadError) await this.finish(runId, 'skipped', 'settings_read_failed')
        else if (error instanceof ScheduleError && error.code === 'schedule_conflict') {
          if (attempt === 0) continue
          await this.store.update(runs => { runs.find(run => run.run_id === runId)!.detail = 'binding_conflict' })
          return 'unbound'
        } else if (error instanceof ScheduleError && ['user_only_field', 'invalid_document'].includes(error.code)) {
          await this.finish(runId, 'failed', `binding_rejected: ${error.code}`)
        } else await this.finish(runId, 'skipped', 'binding_unconfirmed')
        return
      }
      if (typeof result === 'number') return result
      await this.finish(runId, 'skipped', result)
      return
    }
  }

  private async submit(snapshot: Job, accepted: Run) {
    if (!await this.check(snapshot, accepted.run_id)) return
    let requested
    try { requested = effective(snapshot) }
    catch (error) {
      if (!(error instanceof ScheduleError)) throw error
      await this.finish(accepted.run_id, 'failed', error.code)
      return
    }
    let sessionId: string
    if (snapshot.mode === 'create' || !snapshot.session_id) {
      let reply
      try {
        reply = await this.sessions.launch({ cwd: snapshot.cwd, policy: requested,
          ...(snapshot.agent_id ? { agent: snapshot.agent_id } : {}), ...(snapshot.model ? { model: snapshot.model } : {}),
          ...(snapshot.created_by === 'user' ? {} : { starter: {
            sessionId: snapshot.created_by.tool.session_id, branchId: snapshot.created_by.tool.branch_id,
          } }),
        })
      } catch {
        await this.finish(accepted.run_id, 'lost', 'launch_reply_unknown')
        return
      }
      if (!reply.ok) {
        await this.finish(accepted.run_id, 'failed', reply.error.code)
        return
      }
      sessionId = reply.value.sessionId
      await this.store.update(runs => Object.assign(runs.find(run => run.run_id === accepted.run_id)!, {
        session_id: sessionId, session_origin: 'launch', policy: reply.value.policy,
      }))
    } else {
      let live, actual
      try {
        live = snapshot.created_by === 'user' ? await this.jobs.permissions.permissionDefaults({})
          : (await this.jobs.permissions.permission({ sessionId: snapshot.created_by.tool.session_id, branchId: snapshot.created_by.tool.branch_id })).policy
      } catch {
        if (await this.check(snapshot, accepted.run_id)) await this.finish(accepted.run_id, 'skipped', 'policy_read_failed')
        return
      }
      try {
        actual = (await this.jobs.permissions.permission({ sessionId: snapshot.session_id, branchId: 'main' })).policy
      } catch (error) {
        const missing = error !== null && typeof error === 'object' && 'code' in error
          && (error.code === 'session_closed' || error.code === 'branch_missing') ? error.code : undefined
        if (await this.check(snapshot, accepted.run_id)) await this.finish(accepted.run_id,
          missing ? 'failed' : 'skipped', missing ?? 'policy_read_failed')
        return
      }
      if (!within(actual, requested) || !within(actual, live)) {
        if (await this.check(snapshot, accepted.run_id)) await this.finish(accepted.run_id, 'skipped', 'policy')
        return
      }
      sessionId = snapshot.session_id
      await this.store.update(runs => Object.assign(runs.find(run => run.run_id === accepted.run_id)!, {
        session_id: sessionId, session_origin: 'reuse', policy: actual,
      }))
    }
    let revision = snapshot.revision, binding = snapshot.session_id
    if (snapshot.mode === 'reuse' && !snapshot.session_id) {
      const bound = await this.bind(snapshot, accepted.run_id, sessionId)
      if (bound === undefined) return
      if (bound !== 'unbound') { revision = bound; binding = sessionId }
    }
    if (!await this.check(snapshot, accepted.run_id, revision, binding)) return
    let receipt, inputReply
    try {
      inputReply = await this.sessions.input({ sessionId, branchId: 'main', mode: 'queue', submissionId: accepted.submission_id,
        rawBlocks: [{ text: `<dim-scheduler kind="scheduled-run" job="${escaped(snapshot.id)}" run="${escaped(accepted.run_id)}">${escaped(snapshot.name)}</dim-scheduler>` }],
        content: [{ type: 'text', text: snapshot.prompt }],
      })
 } catch { }
    if (inputReply?.ok) receipt = inputReply.value
    else if (inputReply && !['reloading', 'publication_unknown', 'ambiguous'].includes(inputReply.error.code)) {
      await this.finish(accepted.run_id, 'failed', inputReply.error.code)
      return
    }
    if (!receipt) {
      const current = (await this.store.read()).find(run => run.run_id === accepted.run_id)!
      receipt = await this.lookup(current)
      if (!receipt) return
    }
    await this.store.update(runs => Object.assign(runs.find(run => run.run_id === accepted.run_id)!, {
      state: 'submitted', input_event_id: receipt.inputId,
    }))
    return { sessionId, receipt }
  }

  private async lookup(run: Run) {
    while (!this.closing) {
      let reply
      try { reply = await this.sessions.lookup({ sessionId: run.session_id!, branchId: 'main', submissionId: run.submission_id }) }
      catch { await pause(1000); continue }
      if (reply.ok) {
        if (reply.value.status === 'accepted') return reply.value
        await this.finish(run.run_id, 'lost', 'input_unknown')
        return
      }
      if (reply.error.code !== 'reloading') { await this.finish(run.run_id, 'lost', reply.error.code); return }
      await pause(1000)
    }
  }

  private async resume(run: Run) {
    if (!run.session_id) { await this.finish(run.run_id, 'lost', 'launch_identity_unknown'); return }
    if (!run.input_event_id) {
      const found = await this.lookup(run)
      if (!found) return
      await this.store.update(runs => Object.assign(runs.find(value => value.run_id === run.run_id)!, { state: 'submitted', input_event_id: found.inputId }))
      await this.follow({ ...run, input_event_id: found.inputId }, found.sequence)
    } else await this.follow(run)
  }

  private async follow(run: Run, after = 0) {
    const history = new RunHistory(this.sessions, run.session_id!, run.input_event_id!, after)
    let stopIssued = false
    while (!this.closing && !this.fault) {
      if (!await history.read()) { await pause(1000); continue }
      if (history.outcome) {
        await this.finish(run.run_id, stopIssued && history.outcome.state === 'cancelled' ? 'timed_out' : history.outcome.state, history.outcome.detail)
        return
      }
      if (Date.now() >= Date.parse(run.deadline_at!)) {
        if (history.applied && history.interrupted && !history.consumed) {
          await this.finish(run.run_id, 'timed_out', 'interrupted_before_consumption')
          return
        }
        if (!history.applied && !history.consumed) {
          let withdrawn
          try { withdrawn = await this.sessions.withdraw({ sessionId: run.session_id!, branchId: 'main', inputId: run.input_event_id!, reason: 'scheduled run timed out' }) }
 catch { }
          if (withdrawn?.ok && withdrawn.value.status === 'deleted') { await this.finish(run.run_id, 'timed_out', 'withdrawn_at_deadline'); return }
          await history.read()
        } else if (history.consumed && !stopIssued) {
          stopIssued = true
          try {
            const stopped = await this.sessions.stop({ sessionId: run.session_id!, branchId: 'main', reason: 'scheduled run timed out' })
            if (!stopped.ok && stopped.error.code === 'reloading') stopIssued = false
          }
 catch { }
        }
      } else if (history.consumed && run.state !== 'running') {
        await this.store.update(runs => { runs.find(value => value.run_id === run.run_id)!.state = 'running' })
        run.state = 'running'
      }
      const remaining = Date.parse(run.deadline_at!) - Date.now()
      await pause(remaining > 0 ? Math.min(1000, remaining) : 1000)
    }
  }

  private async finish(runId: string, state: Run['state'], detail: string) {
    const run = await this.store.update(runs => {
      const run = runs.find(run => run.run_id === runId)!
      if (terminal(run)) return undefined
      run.state = state
      run.detail = run.detail === 'binding_conflict' ? `binding_conflict${detail ? `; ${detail}` : ''}` : detail
      run.ended_at = new Date(Date.now()).toISOString()
      return run
    })
    if (!run?.report_to || !['completed', 'failed', 'cancelled', 'timed_out', 'lost'].includes(run.state)) return
    let failure: string | undefined
    try {
      const body = `Scheduled job ${run.job_id}, run ${run.run_id}: ${run.state} (${run.detail}).${run.session_id ? ` Session ${run.session_id}.` : ''}\n${this.reportGuidance}`
      const reply = await this.sessions.input({ sessionId: run.report_to.session_id, branchId: run.report_to.branch_id,
        mode: 'steer', submissionId: `scheduler-report:${run.run_id}`, rawBlocks: [{ text:
          `<dim-scheduler kind="report" job="${escaped(run.job_id)}" run="${escaped(run.run_id)}">${escaped(body)}</dim-scheduler>`,
        }], content: [],
      })
      if (!reply.ok) failure = reply.error.code
    } catch { failure = 'bridge_error' }
    if (failure) await this.store.update(runs => {
      runs.find(value => value.run_id === runId)!.detail += `; report_failed: ${failure}`
    })
  }

  async prune() {
    if (this.pruning) {
      this.retryPruneAt ??= Date.now()
      return
    }
    this.pruning = true
    this.retryPruneAt = undefined
    try {
      const runs = await this.store.read()
      const { document } = await this.jobs.read()
      const keep = new Set(runs.filter(run => !terminal(run) || this.work.has(run.run_id)).map(run => run.run_id))
      for (const job of document.jobs) {
        const newest = runs.filter(run => run.job_id === job.id && terminal(run))
          .sort((a, b) => b.ended_at!.localeCompare(a.ended_at!) || b.run_id.localeCompare(a.run_id)).slice(0, 100)
        for (const run of newest) keep.add(run.run_id)
      }
      const references = new Set([...document.jobs.filter(job => job.mode === 'reuse').map(job => job.session_id),
        ...runs.filter(run => keep.has(run.run_id)).map(run => run.session_id)].filter((id): id is string => id !== undefined))
      for (const sessionId of references) {
        const ownership = runs.filter(run => run.session_id === sessionId && run.session_origin === 'launch')
          .sort((a, b) => b.created_at.localeCompare(a.created_at) || b.run_id.localeCompare(a.run_id))[0]
        if (ownership) keep.add(ownership.run_id)
      }
      const expired = runs.filter(run => !keep.has(run.run_id))
      const deletions = new Set(expired.filter(run => run.session_origin === 'launch' && !references.has(run.session_id!)).map(run => run.session_id!))
      for (const sessionId of deletions) {
        try {
          const reply = await this.sessions.delete({ sessionId })
          if (!reply.ok) throw new ScheduleError(reply.error.code)
        } catch (error) {
          this.retryPruneAt = Date.now() + 60_000
          for (const run of expired.filter(run => run.session_id === sessionId && run.session_origin === 'launch')) keep.add(run.run_id)
          console.error('Scheduler cleanup', error instanceof Error ? error.message : String(error))
        }
      }
      const remove = new Set(expired.filter(run => !keep.has(run.run_id)).map(run => run.run_id))
      if (remove.size) await this.store.update(current => {
        for (let index = current.length - 1; index >= 0; index--) if (remove.has(current[index].run_id) && !this.work.has(current[index].run_id)) current.splice(index, 1)
      })
    } catch (error) {
      this.retryPruneAt = Date.now() + 60_000
      console.error('Scheduler retention', error instanceof Error ? error.message : String(error))
    } finally { this.pruning = false }
  }
}
