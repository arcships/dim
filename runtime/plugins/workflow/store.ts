import { z } from 'zod'
import type { FileScope, PermissionPolicy } from '../../../npm/plugin.js'
import { definitionSchema, issues, stages, WorkflowError } from './definition.ts'

const id = z.string().min(1)
const timestamp = z.iso.datetime({ precision: 3 })
const policy = z.strictObject({
  preset: z.enum(['read-only', 'workspace-write', 'full-access']),
  fileAccess: z.enum(['read-only', 'workspace-read-write', 'unrestricted']).optional(),
  toolApproval: z.enum(['ask', 'bypass']).optional(),
  allowedTools: z.array(z.string()), disallowedTools: z.array(z.string()),
}) satisfies z.ZodType<PermissionPolicy>
const address = z.strictObject({ session_id: id, branch_id: id })
const stageSchema = z.strictObject({
  id, step: z.number().int().min(0).max(31),
  state: z.enum(['waiting', 'launching', 'running', 'completed', 'failed', 'stopped', 'skipped', 'lost']),
  session_id: id.optional(), agent_id: id.optional(), submission_id: id.optional(),
  input_event_id: id.optional(), result_text: z.string().refine(text => Buffer.byteLength(text) <= 64 * 1024).optional(),
  reason: z.string().optional(),
})
const runSchema = z.strictObject({
  run_id: id, name: id, source: z.enum(['user', 'project', 'plugin']), definition: definitionSchema,
  params: z.record(z.string(), z.string()), cwd: id, policy_cap: policy.optional(),
  report_to: address.optional(),
  started_by: z.union([
    z.literal('management'),
    z.strictObject({ kind: z.literal('tool'), session_id: id, branch_id: id, call_id: id }),
  ]),
  created_at: timestamp, ended_at: timestamp.optional(),
  state: z.enum(['running', 'stopping', 'completed', 'failed', 'stopped', 'interrupted']),
  stages: z.array(stageSchema).min(1).max(256),
  log: z.array(z.strictObject({ at: timestamp, message: z.string() })).max(500),
  report: z.discriminatedUnion('state', [
    z.strictObject({ state: z.literal('none') }),
    z.strictObject({ state: z.literal('pending'), submission_id: id, attempt_open: z.boolean() }),
    z.strictObject({ state: z.literal('delivered') }),
    z.strictObject({ state: z.literal('undeliverable'), reason: z.string() }),
  ]),
}).superRefine((run, context) => {
  const expected = run.definition.steps.flatMap((step, index) => stages(step).map(stage => ({ id: stage.id, step: index })))
  const invalid = issues(run.definition, run.name).length
    || expected.length !== run.stages.length
    || expected.some((stage, index) => stage.id !== run.stages[index].id || stage.step !== run.stages[index].step)
    || terminal(run) !== (run.ended_at !== undefined)
    || (run.report.state !== 'none' && (!terminal(run) || !run.report_to || run.state === 'stopped'))
    || (run.stages.some(stage => stage.session_id) && !run.policy_cap)
    || run.stages.some(stage => (stage.input_event_id && (!stage.session_id || !stage.submission_id))
      || (stage.state === 'running' && !stage.input_event_id))
  if (invalid) context.addIssue({ code: 'custom', message: 'Inconsistent Workflow run record' })
})
const storedSchema = z.strictObject({ version: z.literal(2), runs: z.array(runSchema) }).superRefine((stored, context) => {
  if (new Set(stored.runs.map(run => run.run_id)).size !== stored.runs.length) {
    context.addIssue({ code: 'custom', message: 'Duplicate Workflow run identity' })
  }
})
export type Run = z.infer<typeof runSchema>
export type Stage = z.infer<typeof stageSchema>

export function terminal(run: { state: string }): boolean {
  return run.state !== 'running' && run.state !== 'stopping'
}

export function stageTerminal(stage: Stage): boolean {
  return stage.state !== 'waiting' && stage.state !== 'launching' && stage.state !== 'running'
}

export function log(run: Run, message: string) {
  run.log.push({ at: new Date().toISOString(), message })
  if (run.log.length > 500) run.log.splice(0, run.log.length - 500)
}

export function bounded(text: string, limit: number): string {
  const bytes = Buffer.from(text)
  if (bytes.length <= limit) return text
  let end = limit
  while ((bytes[end] & 0xc0) === 0x80) end--
  return bytes.subarray(0, end).toString('utf8')
}

/** The only decoder for the committed inner-v2 run document. */
export class RunStore {
  readonly scope: FileScope
  constructor(scope: FileScope) { this.scope = scope }

  async read(): Promise<Run[]> { return this.access(runs => runs, false) }
  async update<T>(change: (runs: Run[]) => T): Promise<T> { return this.access(change, true) }

  private async access<T>(change: (runs: Run[]) => T, write: boolean): Promise<T> {
    let value!: T
    let problem: unknown
    const outcome = await this.scope.withLock('workflow/runs.json', async file => {
      try {
        const text = await file.read()
        if (!text.ok) throw new WorkflowError(text.error)
        let runs: Run[]
        if (text.value === null) runs = []
        else {
          try { runs = storedSchema.parse(JSON.parse(text.value)).runs }
          catch { throw new WorkflowError('invalid_data') }
        }
        value = change(runs)
        if (write) {
          const reply = await file.replace(JSON.stringify({ version: 2, runs }))
          if (!reply.ok) throw new WorkflowError(reply.error)
        }
      } catch (error) {
        problem = error
      }
      return null
    })
    if (!outcome.ok) throw new WorkflowError(outcome.error)
    if (problem !== undefined) throw problem
    return value
  }
}
