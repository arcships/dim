import { ProtocolError, readPermissionPolicy } from "./read"
import type { PermissionPolicy, PermissionPreset } from "./protocol"

export type Schedule = { every_minutes: number } | { at: string } | { cron: string; tz?: string }
type Origin = "user" | { tool: { session_id: string; branch_id: string } }
export type InputMode = "queue" | "steer" | "append"
/** Where a job delivers (Scheduler §2.1): a Session branch, or a new Session. */
export type Target =
  | { kind: "session"; session_id: string; branch_id: string; input_mode: InputMode }
  | { kind: "launch"; cwd: string; agent_id?: string; policy?: PermissionPreset; max_run_minutes: number }
/** Why the owner disabled a job (Scheduler A.3). */
export type DisabledReason = { code: string; at: string }
export type ScheduledRun = {
  run_id: string; job_id: string; state: string; created_at: string; ended_at?: string
  detail: string; session_id?: string; policy?: PermissionPolicy
}
export type ScheduledJob = {
  id: string; name: string; prompt: string; schedule: Schedule; target: Target; enabled: boolean
  enabled_at?: string; disabled_reason?: DisabledReason; revision: number; created_by: Origin
  next_window?: string; schedule_error?: string; last_run?: ScheduledRun
}
export type JobInput = Pick<ScheduledJob, "name" | "prompt" | "schedule" | "target" | "enabled">

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new ProtocolError("Invalid Scheduler reply: expected object")
  return value as Record<string, unknown>
}
function string(value: unknown): string {
  if (typeof value !== "string") throw new ProtocolError("Invalid Scheduler reply: expected string")
  return value
}
function integer(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) throw new ProtocolError("Invalid Scheduler reply: expected nonnegative integer")
  return value
}
function optional<T>(value: unknown, read: (value: unknown) => T): T | undefined { return value === undefined || value === null ? undefined : read(value) }
function list<T>(value: unknown, read: (value: unknown) => T): T[] {
  if (!Array.isArray(value)) throw new ProtocolError("Invalid Scheduler reply: expected array")
  return value.map(read)
}
function preset(value: unknown): PermissionPreset {
  if (value === "read-only" || value === "workspace-write" || value === "full-access") return value
  throw new ProtocolError("Invalid Scheduler permission preset")
}
function policy(value: unknown): PermissionPolicy { return readPermissionPolicy(value, "schedule.policy") }
function branch(value: unknown): { session_id: string; branch_id: string } {
  const v = object(value)
  return { session_id: string(v.session_id), branch_id: string(v.branch_id) }
}
function origin(value: unknown): Origin { return value === "user" ? value : { tool: branch(object(value).tool) } }
function inputMode(value: unknown): InputMode {
  if (value === undefined || value === null || value === "queue") return "queue"
  if (value === "steer" || value === "append") return value
  throw new ProtocolError("Invalid Scheduler input mode")
}
function target(value: unknown): Target {
  const v = object(value)
  if (v.kind === "session") return { kind: "session", session_id: string(v.session_id), branch_id: v.branch_id === undefined || v.branch_id === null ? "main" : string(v.branch_id), input_mode: inputMode(v.input_mode) }
  if (v.kind === "launch") return { kind: "launch", cwd: string(v.cwd), agent_id: optional(v.agent_id, string), policy: optional(v.policy, preset), max_run_minutes: v.max_run_minutes === undefined || v.max_run_minutes === null ? 60 : integer(v.max_run_minutes) }
  throw new ProtocolError("Invalid Scheduler target")
}
function disabledReason(value: unknown): DisabledReason { const v = object(value); return { code: string(v.code), at: string(v.at) } }
function schedule(value: unknown): Schedule {
  const v = object(value)
  if ("every_minutes" in v) return { every_minutes: integer(v.every_minutes) }
  if ("at" in v) return { at: string(v.at) }
  return { cron: string(v.cron), ...(v.tz == null ? {} : { tz: string(v.tz) }) }
}
export function readScheduledRun(value: unknown): ScheduledRun {
  const v = object(value)
  return { run_id: string(v.run_id), job_id: string(v.job_id), state: string(v.state), created_at: string(v.created_at), ended_at: optional(v.ended_at, string), detail: v.detail === undefined || v.detail === null ? "" : string(v.detail), session_id: optional(v.session_id, string), policy: optional(v.policy, policy) }
}
function job(value: unknown): ScheduledJob {
  const v = object(value)
  if (typeof v.enabled !== "boolean") throw new ProtocolError("Invalid Scheduler job state")
  return {
    id: string(v.id), name: string(v.name), prompt: string(v.prompt), schedule: schedule(v.schedule), target: target(v.target), enabled: v.enabled,
    enabled_at: optional(v.enabled_at, string), disabled_reason: optional(v.disabled_reason, disabledReason),
    revision: integer(v.revision), created_by: origin(v.created_by), next_window: optional(v.next_window, string), schedule_error: optional(v.schedule_error, string), last_run: optional(v.last_run, readScheduledRun),
  }
}
export function readScheduledJobs(value: unknown): ScheduledJob[] { return list(object(value).jobs, job) }
export function readScheduledHistory(value: unknown): ScheduledRun[] { return list(object(value).runs, readScheduledRun) }
export function readScheduledJob(value: unknown): ScheduledJob { return job(object(value).job) }
export function readScheduledSave(value: unknown): { job: ScheduledJob } { return { job: job(object(value).job) } }
export function readScheduledDelete(value: unknown): string { return string(object(value).job_id) }
