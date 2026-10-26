import { definePlugin } from '@arcships/rutis'
import { z } from 'zod'
import type { ManagementMethods, MethodReply, PartRegistry, PluginFiles, Sessions, Settings, WorkflowFiles, SourceDeclaration } from '../../../npm/plugin.js'
import declarations from './settings.json' with { type: 'json' }
import { Catalog, type DefinitionEntry } from './catalog.ts'
import { WorkflowError } from './definition.ts'
import { Workflow } from './engine.ts'
import { management, unloadingAfterCall, type RunReply, type RunRequest, type WorkflowRuns } from './requests.ts'
import session from './session.ts'
import { bounded, RunStore, type Run } from './store.ts'

const options = z.strictObject({ packages: z.array(z.strictObject({ id: z.string().min(1), root: z.string().min(1) })) })
const provides = { workflowRuns: { call: 'async' } } as const

function visible(entries: DefinitionEntry[]) {
  return entries.map(entry => ({ name: entry.name, source: entry.source, enabled: entry.enabled,
    ...(entry.package_id ? { package_id: entry.package_id } : {}),
    ...(entry.definition ? { description: entry.definition.description, parameters: entry.definition.parameters ?? [] } : {}),
    valid: entry.issues.length === 0, issues: entry.issues.slice(0, 10), issue_count: entry.issues.length,
  }))
}

export default definePlugin({
  name: 'Workflow',
  config: z.toJSONSchema(options),
  inject: ['sessions', 'settings', 'pluginFiles', 'workflowFiles', 'managementMethods', 'partRegistry'],
  provides,
  async apply(ctx, config: z.infer<typeof options>) {
    for (const declaration of declarations as SourceDeclaration[]) {
      await ctx.use<Settings>('settings').register(declaration)
    }
    const catalog = new Catalog(ctx.use<Settings>('settings'), ctx.use<WorkflowFiles>('workflowFiles'), config.packages)
    const scope = await ctx.use<PluginFiles>('pluginFiles').open()
    const workflow = new Workflow(catalog, ctx.use<Sessions>('sessions'), new RunStore(scope))
    ctx.effect(() => workflow.close())
    try { await workflow.restore() }
    catch (error) {
      workflow.fault = error instanceof Error ? error : new Error(String(error))
      console.error('Workflow', workflow.fault.message)
    }

    const owner: WorkflowRuns = {
      async call(request: RunRequest) {
        try {
          return await workflow.track(async (): Promise<RunReply> => {
            if (request.action === 'list') {
              const listed = visible((await catalog.load(request.caller.cwd)).entries.filter(entry => entry.enabled))
              return { content: JSON.stringify(listed), structured: { definitions: listed } }
            }
            let result: Run
            if (request.action === 'start') {
              result = await workflow.start({ ...request, cwd: request.caller.cwd }, () => false)
              const value = { run_id: result.run_id, status: result.state, stages: result.stages }
              return { content: `${JSON.stringify(value)}\nThe result arrives as a report input.`, structured: value }
            }
            if (request.action === 'status') result = await workflow.status(request.run_id, request.caller.sessionId)
            else result = await workflow.stop(request.run_id, request.caller.sessionId)
            const status = { run_id: result.run_id, state: result.state,
              stages: result.stages.map(({ id, state, session_id }) => ({ id, state, session_id })),
              result_preview: bounded(result.stages.map(stage => `${stage.id}: ${stage.result_text ?? stage.reason ?? stage.state}`).join('\n'), 4 * 1024),
              log: result.log.slice(-10).map(entry => ({ at: entry.at, message: bounded(entry.message, 512) })),
            }
            return { content: JSON.stringify(status), structured: { run: result } }
          })
        } catch (error) {
          if (error instanceof WorkflowError && error.code === 'service_reloading') {
            return { code: 'service_unavailable', message: unloadingAfterCall }
          }
          if (error instanceof WorkflowError) return { code: error.code, message: bounded(error.message, 4 * 1024) }
          throw error
        }
      },
    }
    ctx.provide('workflowRuns', owner)
    // Each Session mounts the `workflow` tool part (session.ts).
    await ctx.use<PartRegistry>('partRegistry').registerDefinition('session', 'WorkflowSession', session)

    await ctx.use<ManagementMethods>('managementMethods').register(
      'workflow',
      ['list', 'save', 'start', 'status', 'stop'].map(name => ({
        name: `workflow.${name}`, mutating: !['list', 'status'].includes(name),
        interruption: name === 'list' || name === 'status' ? 'report' : 'drain', progress: false,
      })),
      async (call, cancellation) => {
        let cancelled = false
        void cancellation.then(() => { cancelled = true }, () => { cancelled = true })
        try {
          return await workflow.track<MethodReply>(async () => {
            const name = call.method.slice('workflow.'.length)
            if (name === 'list') {
              const { cwd } = management.list.parse(call.params)
              return { kind: 'value' as const, value: visible((await catalog.load(cwd)).entries) }
            }
            if (name === 'save') {
              const { source, cwd, document } = management.save.parse(call.params)
              return { kind: 'value' as const, value: await catalog.save(source, cwd, document) }
            }
            if (name === 'start') {
              const request = management.start.parse(call.params)
              return { kind: 'value' as const, value: { run_id: (await workflow.start(request, () => cancelled)).run_id } }
            }
            if (name === 'status') return { kind: 'value' as const, value: await workflow.status(management.status.parse(call.params).run_id) }
            if (name === 'stop') return { kind: 'value' as const, value: await workflow.stop(management.stop.parse(call.params).run_id) }
            throw new WorkflowError('invalid_params', 'Unknown Workflow method')
          })
        } catch (error) {
          if (error instanceof z.ZodError) return { code: 'invalid_params', message: bounded(error.message, 4 * 1024) }
          if (error instanceof WorkflowError) return { code: error.code, message: bounded(error.message, 4 * 1024) }
          throw error
        }
      },
    )
  },
})
