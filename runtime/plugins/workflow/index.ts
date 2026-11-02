import { definePlugin } from '@arcships/rutis'
import { z } from 'zod'
import type { ManagementMethods, MethodReply, PartRegistry, PluginFiles, Sessions, Settings, WorkflowFiles } from '../../../npm/plugin.js'
import { Catalog, type DefinitionEntry } from './catalog.ts'
import { WorkflowError, payloadBytes } from './definition.ts'
import { Workflow } from './engine.ts'
import { management, unloadingAfterCall, type RunReply, type RunRequest, type WorkflowRuns } from './requests.ts'
import session from './session.ts'
import { settings } from './settings.ts'
import { bounded, boundedEncoded, RunStore, type Run } from './store.ts'

/// `report_guidance` is one line: the entry names the report's last line as
/// guidance, not part of the workflow's account, so a value with a line
/// break is refused.
const guidance = z.string().min(1).refine(value => !/[\r\n]/.test(value), {
  message: 'report_guidance must be one line: the entry names the report\'s last line as guidance',
})
const options = z.strictObject({
  packages: z.array(z.strictObject({ id: z.string().min(1), root: z.string().min(1) })),
  report_guidance: guidance.optional(),
  invalid_definition_guidance: z.string().min(1).optional(),
})
const provides = { workflowRuns: { call: 'async' } } as const

/** A listed field's preview: its serialized form stays within `limit`, and a cut
 * text ends with an ellipsis so it is not read as the whole value. */
function preview(text: string, limit: number): string {
  return boundedEncoded(text, limit) === text ? text : `${boundedEncoded(text, limit - 8)}…`
}

function visible(entry: DefinitionEntry) {
  return { name: entry.name, source: entry.source, enabled: entry.enabled,
    ...(entry.package_id ? { package_id: entry.package_id } : {}),
    ...(entry.definition ? {
      description: preview(entry.definition.description, 4 * 1024),
      parameters: (entry.definition.parameters ?? []).map(parameter => ({
        name: parameter.name,
        ...(parameter.required === undefined ? {} : { required: parameter.required }),
        ...(parameter.description === undefined ? {} : { description: preview(parameter.description, 512) }),
        ...(parameter.default === undefined ? {} : { default: preview(parameter.default, 1024) }),
      })),
    } : {}),
    valid: entry.issues.length === 0,
    issues: entry.issues.slice(0, 10).map(issue => preview(issue, 1024)),
    issue_count: entry.issues.length,
  }
}

/** One reply's listing: field previews, and entries while the serialized payload fits.
 * `truncated` says later entries were omitted for the frame bound. */
function listing(entries: DefinitionEntry[]) {
  const definitions: ReturnType<typeof visible>[] = []
  let bytes = Buffer.byteLength(JSON.stringify('{"definitions":[],"truncated":false}'))
  let truncated = false
  for (const entry of entries) {
    const value = visible(entry)
    const part = `${definitions.length ? ',' : ''}${JSON.stringify(value)}`
    const size = Buffer.byteLength(JSON.stringify(part)) - 2
    if (bytes + size + 64 > payloadBytes) { truncated = true; break }
    definitions.push(value)
    bytes += size
  }
  return { definitions, truncated }
}

export default definePlugin({
  name: 'Workflow',
  // A plain JSON Schema: zod's result also carries `~standard`, which
  // rutis refuses as a Standard Schema.
  config: JSON.parse(JSON.stringify(z.toJSONSchema(options))),
  inject: ['sessions', 'settings', 'pluginFiles', 'workflowFiles', 'managementMethods', 'partRegistry'],
  provides,
  async apply(ctx, config: z.infer<typeof options>) {
    config = options.parse(config)
    await ctx.use<Settings>('settings').register(settings)
    const catalog = new Catalog(ctx.use<Settings>('settings'), ctx.use<WorkflowFiles>('workflowFiles'), config.packages)
    const scope = await ctx.use<PluginFiles>('pluginFiles').open()
    const workflow = new Workflow(catalog, ctx.use<Sessions>('sessions'), new RunStore(scope), config.report_guidance)
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
              return { content: JSON.stringify(listing(
                (await catalog.load(request.caller.cwd)).entries.filter(entry => entry.enabled))) }
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
    await ctx.use<PartRegistry>('partRegistry').registerDefinition('session', 'WorkflowSession', session(config.invalid_definition_guidance))

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
              return { kind: 'value' as const, value: listing((await catalog.load(cwd)).entries) }
            }
            if (name === 'save') {
              return { kind: 'value' as const, value: await catalog.save(management.save.parse(call.params)) }
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
