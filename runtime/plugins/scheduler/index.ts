import { definePlugin } from '@arcships/rutis'
import { z } from 'zod'
import type { JsonObject, ManagementMethods, PartRegistry, PluginFiles, SessionPermissions, Sessions, Settings, SourceDeclaration } from '../../../npm/plugin.js'
import declarations from './settings.json' with { type: 'json' }
import { ScheduleError, type Job } from './data.ts'
import { Scheduler } from './engine.ts'
import { Jobs } from './jobs.ts'
import { bounded, management, unloadingBeforeCall, type ScheduleJobs } from './requests.ts'
import session from './session.ts'
import { RunStore, type Run } from './store.ts'
import { windowAfter } from './windows.ts'

function jobPreview(job: Job) {
  return { ...job, name: bounded(job.name, 256), prompt: bounded(job.prompt, 4 * 1024),
    prompt_truncated: Buffer.byteLength(job.prompt) > 4 * 1024 }
}

function runPreview(run: Run) { return { ...run, detail: bounded(run.detail, 512) } }

export default definePlugin({
  name: 'Scheduler', config: z.toJSONSchema(z.strictObject({})),
  inject: ['sessions', 'sessionPermissions', 'settings', 'pluginFiles', 'managementMethods', 'partRegistry'],
  provides: { scheduleJobs: { call: 'async' } },
  async apply(ctx) {
    for (const declaration of declarations as SourceDeclaration[]) {
      await ctx.use<Settings>('settings').register(declaration)
    }
    const jobs = new Jobs(ctx.use<Settings>('settings'), ctx.use<SessionPermissions>('sessionPermissions'))
    const scope = await ctx.use<PluginFiles>('pluginFiles').open()
    const scheduler = new Scheduler(jobs, ctx.use<Sessions>('sessions'), new RunStore(scope))
    ctx.effect(() => scheduler.close())

    async function list() {
      const { document } = await jobs.read()
      const runs = await scheduler.store.read()
      return { jobs: document.jobs.map(job => {
        const ownRuns = runs.filter(run => run.job_id === job.id).sort((a, b) => b.created_at.localeCompare(a.created_at) || b.run_id.localeCompare(a.run_id))
        const windows = ownRuns.filter(run => run.window !== 'manual').map(run => run.window).sort()
        const after = windows.length ? Date.parse(windows.at(-1)!)
          : 'at' in job.schedule ? Number.NEGATIVE_INFINITY : Date.parse(job.enabled_at!) - 1
        let next_window: string | null = null, issue: string | undefined
        try {
          const next = job.enabled ? windowAfter(job, after) : undefined
          if (next !== undefined) next_window = new Date(next).toISOString()
        }
        catch (error) { issue = error instanceof Error ? error.message : String(error) }
        return { ...job, next_window,
          ...(issue ? { schedule_error: issue } : {}), last_run: ownRuns[0] ?? null }
      }) }
    }
    const history = async (jobId: string, limit: number) => ({ runs: (await scheduler.store.read()).filter(run => run.job_id === jobId)
      .sort((a, b) => b.created_at.localeCompare(a.created_at) || b.run_id.localeCompare(a.run_id)).slice(0, limit) })
    async function remove(jobId: string) {
      const result = await jobs.delete(jobId)
      await scheduler.prune()
      return result
    }

    const owner: ScheduleJobs = {
      async call(request) {
        try {
          return await scheduler.track(async () => {
            let records: JsonObject, visible: JsonObject
            if (request.action === 'list') {
              const listed = await list()
              records = listed
              visible = { jobs: listed.jobs.map(({ prompt, ...job }) => ({ ...job,
                name: bounded(job.name, 256), last_run: job.last_run ? runPreview(job.last_run) : null,
                ...(job.schedule_error ? { schedule_error: bounded(job.schedule_error, 512) } : {}),
              })) }
            } else if (request.action === 'get') {
              const job = await jobs.get(request.job_id)
              records = { job }; visible = { job: jobPreview(job) }
            }
            else if (request.action === 'create' || request.action === 'update') {
              const saved = await jobs.save({ ...request.job, ...(request.action === 'update' ? { id: request.job_id } : {}) }, request.caller)
              records = saved; visible = { ...saved, job: jobPreview(saved.job) }
            } else if (request.action === 'set_enabled') {
              const job = await jobs.setEnabled(request.job_id, request.enabled)
              records = { job }; visible = { job: jobPreview(job) }
            } else if (request.action === 'delete') visible = records = await remove(request.job_id)
            else if (request.action === 'run_now') {
              const run = await scheduler.accept(request.job_id)
              records = run; visible = runPreview(run)
            } else {
              const result = await history(request.job_id, request.limit)
              records = result; visible = { runs: result.runs.map(runPreview) }
            }
            return { content: JSON.stringify(visible), records }
          })
        } catch (error) {
          if (error instanceof ScheduleError && error.code === 'service_reloading') {
            return { code: 'service_unavailable', message: unloadingBeforeCall }
          }
          if (error instanceof ScheduleError) return { code: error.code, message: bounded(error.message, 4 * 1024) }
          throw error
        }
      },
    }
    // Keep the namespace known even when restoration fails, without admitting new work during it.
    try { await scheduler.restore() }
    catch (error) { scheduler.fault = error }

    await ctx.use<ManagementMethods>('managementMethods').register(
      'schedule',
      ['list', 'save', 'delete', 'set_enabled', 'run_now', 'history'].map(name => ({
        name: `schedule.${name}`, mutating: name !== 'list' && name !== 'history',
        interruption: name === 'list' || name === 'history' ? 'report' : 'drain', progress: false,
      })),
      async (call, cancellation) => {
        let cancelled = false
        void cancellation.then(() => { cancelled = true }, () => { cancelled = true })
        await Promise.resolve()
        if (cancelled) return { code: 'service_reloading', message: 'Scheduler call cancelled before admission' }
        try {
          return await scheduler.track(async () => {
            let value: JsonObject
            switch (call.method) {
              case 'schedule.list': management.list.parse(call.params); value = await list(); break
              case 'schedule.save': {
                const input = management.save.parse(call.params)
                value = await jobs.save(input.job, undefined, input.revision)
                break
              }
              case 'schedule.delete': value = await remove(management.delete.parse(call.params).job_id); break
              case 'schedule.set_enabled': {
                const input = management.set_enabled.parse(call.params)
                value = { job: await jobs.setEnabled(input.job_id, input.enabled) }
                break
              }
              case 'schedule.run_now': value = await scheduler.accept(management.run_now.parse(call.params).job_id); break
              case 'schedule.history': {
                const input = management.history.parse(call.params)
                value = await history(input.job_id, input.limit)
                break
              }
              default: return { code: 'unknown_method', message: 'Unknown Scheduler method' }
            }
            return { kind: 'value' as const, value }
          })
        } catch (error) {
          if (error instanceof z.ZodError) return { code: 'schedule_invalid', message: error.message }
          if (error instanceof ScheduleError) return { code: error.code, message: error.message }
          throw error
        }
      },
    )
    if (scheduler.fault) throw scheduler.fault
    ctx.provide('scheduleJobs', owner)
    // Each Session mounts the `schedule` tool part (session.ts).
    await ctx.use<PartRegistry>('partRegistry').registerDefinition('session', 'SchedulerSession', session)
    ctx.on('settings/schedule', () => scheduler.changed())
    scheduler.startTimer()
  },
})
