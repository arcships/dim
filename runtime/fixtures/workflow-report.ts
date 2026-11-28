import { createConnection } from 'node:net'
import { once } from 'node:events'
import { definePlugin } from '@arcships/rutis'
import { z } from 'zod'
import type { FileResult, ManagementMethods, PluginFiles, Sessions } from '../../npm/plugin.js'

const runSchema = z.looseObject({
  run_id: z.string(),
  state: z.literal('completed'),
  report_to: z.object({ session_id: z.string(), branch_id: z.string() }),
  report: z.discriminatedUnion('state', [
    z.object({ state: z.literal('pending'), submission_id: z.string(), attempt_open: z.boolean() }),
    z.object({ state: z.literal('delivered') }),
    z.object({ state: z.literal('undeliverable'), reason: z.string() }),
  ]),
})
const storedSchema = z.object({ version: z.literal(2), runs: z.tuple([runSchema]) })
type Run = z.infer<typeof runSchema>
const controlsSchema = z.object({
  pause: z.enum(['opened', 'reply_write']).optional(),
  gate: z.object({ host: z.string(), port: z.number() }).optional(),
})

export default definePlugin({
  name: 'WorkflowReportBindingFixture',
  inject: ['sessions', 'pluginFiles', 'managementMethods'],
  provides: {},
  async apply(ctx) {
    const scope = await ctx.use<PluginFiles>('pluginFiles').open()
    ctx.effect(() => scope.close())
    const sessions = ctx.use<Sessions>('sessions')
    let stopped = false
    let startup = true
    let work: Promise<Run> | undefined

    const store = async (update?: (run: Run) => void, beforeReplace?: () => Promise<void>): Promise<Run> => {
      const result = await scope.withLock<FileResult<Run>>('workflow/runs.json', async file => {
        const text = await file.read()
        if (!text.ok) return text
        let stored: z.infer<typeof storedSchema>
        try { stored = storedSchema.parse(JSON.parse(text.value ?? 'null')) }
        catch { return { ok: false, error: 'invalid_data' } }
        const run = stored.runs[0]
        if (update) {
          update(run)
          await beforeReplace?.()
          const replaced = await file.replace(JSON.stringify(stored))
          if (!replaced.ok) return replaced
        }
        return { ok: true, value: run }
      })
      if (!result.ok) throw new Error(result.error)
      if (!result.value.ok) throw new Error(result.value.error)
      return result.value.value
    }
    await ctx.use<ManagementMethods>('managementMethods').register(
      'report',
      ['status', 'send', 'recover'].map(name => ({
        name: `report.${name}`, mutating: name === 'send' || name === 'recover',
        interruption: 'drain', progress: true,
      })),
      async (call, _cancelled, progress) => {
        try {
          if (call.method === 'report.status') return { kind: 'value', value: { pid: process.pid } }
          if (stopped || work) throw new Error('owner_unavailable')
          const recovering = call.method === 'report.recover'
          if (recovering && !startup) throw new Error('not_startup')
          startup = false
          const controls = controlsSchema.parse(call.params)
          const pause = async (phase: 'opened' | 'reply_write') => {
            if (controls.pause !== phase) return
            if (!controls.gate) throw new Error('missing_gate')
            const socket = createConnection(controls.gate)
            ctx.effect(() => { socket.destroy() })
            try {
              await once(socket, 'connect')
              progress({ phase })
              for await (const _ of socket) break
            } finally { socket.destroy() }
          }
          work = (async () => {
            let run = await store()
            if (run.report.state !== 'pending') return run
            const target = { sessionId: run.report_to.session_id, branchId: run.report_to.branch_id }
            const submissionId = run.report.submission_id
            await store(run => {
              if (run.report.state === 'pending') run.report.attempt_open = true
            })
            await pause('opened')
            progress({ phase: 'input' })
            const text = `<dim-workflow kind="report" run_id="${run.run_id}">completed</dim-workflow>`
            const written = await sessions.call({ sessionId: target.sessionId, lineId: target.branchId,
              service: 'timelineContext', method: 'input',
              args: [{ submission_id: submissionId, mode: 'steer', blocks: [{ type: 'raw', text }] }] })
            const code = written.ok ? undefined : written.error.owner_code ?? written.error.code
            const reply = written.ok ? written : { ok: false as const, error: { code: code!, retryable: written.error.retryable === true } }
            if (!reply.ok && reply.error.code === 'AmbiguousSubmission') {
              const delivered = await store(run => { run.report = { state: 'delivered' } })
              progress({ phase: 'duplicate_report_violation' })
              return delivered
            }
            return store(run => {
              const gone = ['no_session', 'no_line', 'Closed', 'UnknownBranch', 'BranchEnded']
              if (reply.ok) run.report = { state: 'delivered' }
              else if (gone.includes(reply.error.code) || reply.error.code === 'InvalidInput') {
                run.report = { state: 'undeliverable', reason: reply.error.code }
              } else if (run.report.state === 'pending') run.report.attempt_open = false
            }, () => pause('reply_write'))
          })()
          try { return { kind: 'value', value: JSON.stringify(await work) } }
          finally { work = undefined }
        } catch (error) {
          stopped = true
          return { code: error instanceof Error ? error.message : 'fixture_failed', message: 'Report fixture failed' }
        }
      },
    )
  },
})
