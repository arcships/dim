import { randomUUID } from 'node:crypto'
import type { JsonObject, SessionPermissions, Settings } from '../../../npm/plugin.js'
import { authority, ScheduleError, validateJob, type Caller, type Job, type JobPatch, type ScheduleSettings } from './data.ts'

export class SettingsReadError extends ScheduleError {}

export class Jobs {
  readonly settings: Settings
  readonly permissions: SessionPermissions
  constructor(settings: Settings, permissions: SessionPermissions) {
    this.settings = settings; this.permissions = permissions
  }

  async read() {
    let reply
    try { reply = await this.settings.read({ source: 'schedule', scope: 'user' }) }
    catch (error) { throw new SettingsReadError('failed', error instanceof Error ? error.message : String(error)) }
    if (!reply.ok) throw new SettingsReadError(reply.error.code, reply.error.message)
    return { document: reply.value.document as unknown as ScheduleSettings, revision: reply.value.revision }
  }

  async save(patch: JobPatch, caller?: Caller, revision?: number) {
    const snapshot = await this.read()
    const old = snapshot.document.jobs.find(job => job.id === patch.id)
    if (patch.id && !old) throw new ScheduleError('schedule_not_found')
    if (revision !== undefined && revision !== (old?.revision ?? 0)) throw new ScheduleError('schedule_conflict')
    const bound = caller ? (await this.permissions.permission({ sessionId: caller.sessionId, branchId: caller.branchId })).policy
      : await this.permissions.permissionDefaults({})
    const { enabled_at: oldAnchor, ...previous } = old ?? {}
    const candidate = { ...previous, ...patch, id: old?.id ?? patch.id ?? randomUUID(),
      cwd: patch.cwd ?? old?.cwd ?? caller?.cwd, mode: patch.mode ?? old?.mode ?? 'create',
      max_run_minutes: patch.max_run_minutes ?? old?.max_run_minutes ?? 60,
      enabled: patch.enabled ?? old?.enabled ?? true,
      policy_cap: bound, created_by: caller ? { tool: { session_id: caller.sessionId, branch_id: caller.branchId } } : 'user',
      revision: (old?.revision ?? 0) + 1,
    } satisfies Partial<Job>
    if (patch.mode === 'create' && patch.session_id === undefined) delete candidate.session_id
    const { name, schedule, prompt, cwd } = candidate
    if (name === undefined || schedule === undefined || prompt === undefined || cwd === undefined) {
      throw new ScheduleError('schedule_invalid', 'A new job needs name, schedule, prompt and cwd')
    }
    const job: Job = { ...candidate, name, schedule, prompt, cwd,
      ...(candidate.enabled ? { enabled_at: old?.enabled ? oldAnchor! : new Date().toISOString() } : {}),
    }
    validateJob(job)
    const transition = { from: authority(old), to: authority(job) }
    const jobs = old ? snapshot.document.jobs.map(value => value.id === old.id ? job : value) : [...snapshot.document.jobs, job]
    await this.write(snapshot, jobs)
    return { job, transition }
  }

  async get(id: string): Promise<Job> {
    const job = (await this.read()).document.jobs.find(value => value.id === id)
    if (!job) throw new ScheduleError('schedule_not_found')
    return job
  }

  async setEnabled(id: string, enabled: boolean) {
    const snapshot = await this.read()
    const old = snapshot.document.jobs.find(job => job.id === id)
    if (!old) throw new ScheduleError('schedule_not_found')
    const { enabled_at, ...rest } = old
    const job: Job = { ...rest, enabled, revision: old.revision + 1,
      ...(enabled ? { enabled_at: old.enabled ? enabled_at! : new Date().toISOString() } : {}),
    }
    await this.write(snapshot, snapshot.document.jobs.map(value => value.id === id ? job : value))
    return job
  }

  async delete(id: string) {
    const snapshot = await this.read()
    if (!snapshot.document.jobs.some(job => job.id === id)) throw new ScheduleError('schedule_not_found')
    await this.write(snapshot, snapshot.document.jobs.filter(job => job.id !== id))
    return { job_id: id }
  }

  async bind(snapshot: Job, sessionId: string): Promise<number | 'changed' | 'disabled'> {
    const current = await this.read()
    const job = current.document.jobs.find(value => value.id === snapshot.id)
    if (job && !job.enabled) return 'disabled'
    if (!job || job.revision !== snapshot.revision || job.mode !== 'reuse' || job.session_id) return 'changed'
    const saved: Job = { ...job, session_id: sessionId, revision: job.revision + 1 }
    const committed = await this.write(current, current.document.jobs.map(value => value.id === job.id ? saved : value))
    return committed.jobs.find(value => value.id === job.id)!.revision
  }

  async settleWindow(snapshot: Job) {
    const current = await this.read()
    const job = current.document.jobs.find(value => value.id === snapshot.id)
    if (!job || job.revision !== snapshot.revision || !job.enabled) return
    const { enabled_at, ...rest } = job
    const saved: Job = { ...rest, revision: job.revision + 1, enabled: false }
    await this.write(current, current.document.jobs.map(value => value.id === job.id ? saved : value))
  }

  private async write(snapshot: { document: ScheduleSettings; revision: string }, jobs: Job[]) {
    const reply = await this.settings.update({ source: 'schedule', scope: 'user', expectedRevision: snapshot.revision,
      document: { ...snapshot.document, jobs } as unknown as JsonObject,
    })
    if (!reply.ok) throw new ScheduleError(reply.error.code === 'conflict' ? 'schedule_conflict' : reply.error.code, reply.error.message)
    return reply.value.document as unknown as ScheduleSettings
  }
}
