import { CronExpressionParser } from 'cron-parser'
import { z } from 'zod'
import type { PermissionPolicy } from '../../../npm/plugin.js'

export class ScheduleError extends Error {
  readonly code: string
  constructor(code: string, message = code) { super(message); this.code = code }
}

export const id = z.string().min(1)
export const preset = z.enum(['read-only', 'workspace-write', 'full-access'])
const fileAccess = z.enum(['read-only', 'workspace-read-write', 'unrestricted'])
export const policy = z.strictObject({ preset, fileAccess: fileAccess.optional(),
  toolApproval: z.enum(['ask', 'bypass']).optional(),
  allowedTools: z.array(z.string()), disallowedTools: z.array(z.string()) }) satisfies z.ZodType<PermissionPolicy>
export const timestamp = z.iso.datetime({ precision: 3 }).max(24)
export const address = z.strictObject({ session_id: id, branch_id: id })
const cronField = '[0-9*/,-]+'
export const schedule = z.union([
  z.strictObject({ every_minutes: z.number().int().min(1) }),
  z.strictObject({ at: z.iso.datetime({ offset: true }) }),
  z.strictObject({ cron: z.string().regex(new RegExp(`^${Array(5).fill(cronField).join(' +')}$`)), tz: id.optional() }),
])
export const fields = z.strictObject({
  name: id, schedule, prompt: z.string().max(32 * 1024), cwd: id,
  model: z.strictObject({ providerId: id, modelId: id }).optional(),
  mode: z.enum(['create', 'reuse']), policy: preset.optional(), max_run_minutes: z.number().int().min(1),
})
export const jobSchema = fields.extend({
  id, agent_id: id.optional(), session_id: id.optional(),
  policy_cap: policy, created_by: z.union([z.literal('user'), z.strictObject({ tool: address })]),
  enabled: z.boolean(), enabled_at: timestamp.optional(), revision: z.number().int().min(1),
  max_run_minutes: fields.shape.max_run_minutes.default(60),
})
export type Job = z.infer<typeof jobSchema>
export type Schedule = z.infer<typeof schedule>
export interface ScheduleSettings { jobs: Job[]; max_concurrent_runs: number }
export interface Caller { sessionId: string; branchId: string; cwd: string }

export const patchSchema = fields.partial()
export const managementJob = patchSchema.extend({
  id: id.optional(), enabled: z.boolean().optional(), session_id: id.optional(), agent_id: id.optional(),
})
export type JobPatch = z.infer<typeof managementJob>

/** The same conservative policy cap used by S2; lists are sets. */
export function cap(requested: PermissionPolicy, bound: PermissionPolicy) {
  const ranks = preset.options
  const legacyReadOnly = [requested, bound].some(value => approval(value) === 'legacy-read-only')
  if (legacyReadOnly && [requested, bound].some(value => value.toolApproval === 'ask')) {
    throw new ScheduleError('schedule_invalid', 'Explicit Ask cannot be capped by legacy read-only tool rules')
  }
  const disallowedTools = [...new Set([...requested.disallowedTools, ...bound.disallowedTools])]
  const allowedTools = [...new Set([...requested.allowedTools, ...bound.allowedTools])].filter(name =>
    (approval(requested) === 'bypass' || requested.allowedTools.includes(name))
    && (approval(bound) === 'bypass' || bound.allowedTools.includes(name)) && !disallowedTools.includes(name))
  return { preset: ranks[Math.min(ranks.indexOf(requested.preset), ranks.indexOf(bound.preset))],
    ...(requested.fileAccess !== undefined || bound.fileAccess !== undefined ? {
      fileAccess: fileAccess.options[Math.min(fileRank(requested), fileRank(bound))],
    } : {}),
    ...(!legacyReadOnly && (requested.toolApproval !== undefined || bound.toolApproval !== undefined) ? {
      toolApproval: approval(requested) === 'bypass' && approval(bound) === 'bypass' ? 'bypass' as const : 'ask' as const,
    } : {}), allowedTools, disallowedTools } satisfies PermissionPolicy
}

function fileRank(value: PermissionPolicy): number {
  return value.fileAccess === undefined ? preset.options.indexOf(value.preset) : fileAccess.options.indexOf(value.fileAccess)
}

function approval(value: PermissionPolicy) {
  return value.toolApproval ?? (value.preset === 'full-access' ? 'bypass' : value.preset === 'read-only' ? 'legacy-read-only' : 'ask')
}

export function within(actual: PermissionPolicy, bound: PermissionPolicy): boolean {
  let capped: PermissionPolicy
  try { capped = cap(actual, bound) }
  catch (error) { if (error instanceof ScheduleError) return false; throw error }
  return actual.preset === capped.preset
    && fileRank(actual) === fileRank(capped) && approval(actual) === approval(capped)
    && [actual.allowedTools, actual.disallowedTools].every((names, index) => {
      const result = index === 0 ? capped.allowedTools : capped.disallowedTools
      return new Set(names).size === result.length && names.every(name => result.includes(name))
    })
}

export function effective(job: Job) {
  const requested = job.policy ? { preset: job.policy, allowedTools: [], disallowedTools: [] }
    : job.created_by === 'user' ? { preset: 'read-only' as const, allowedTools: [], disallowedTools: [] } : job.policy_cap
  return cap(requested, job.policy_cap)
}

/** Business validation of a candidate authorizing save, before its document CAS. */
export function validateJob(job: Job): void {
  if (Buffer.byteLength(job.prompt) > 32 * 1024) throw new ScheduleError('schedule_invalid', 'Prompt exceeds 32 KiB')
  if (Date.now() + job.max_run_minutes * 60_000 > Date.parse('9999-12-31T23:59:59.999Z')) {
    throw new ScheduleError('schedule_invalid', 'Run deadline exceeds the persisted timestamp range')
  }
  if (job.policy && preset.options.indexOf(job.policy) > preset.options.indexOf(job.policy_cap.preset)) {
    throw new ScheduleError('schedule_invalid', 'Job policy exceeds the saving authority')
  }
  effective(job)
  if (job.session_id && job.mode !== 'reuse') throw new ScheduleError('schedule_invalid', 'Only reuse jobs bind an existing Session')
  if ('cron' in job.schedule) {
    try {
      if (job.schedule.tz) new Intl.DateTimeFormat('en', { timeZone: job.schedule.tz }).format(0)
      CronExpressionParser.parse(job.schedule.cron, { tz: job.schedule.tz }).next()
    } catch { throw new ScheduleError('schedule_invalid', 'Invalid cron expression or time zone') }
  }
}

export function authority(job: Job | undefined) {
  return job ? { origin: job.created_by, cap: job.policy_cap, effective_policy: effective(job),
    report_to: job.created_by === 'user' ? null : job.created_by.tool } : null
}
