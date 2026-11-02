import { z } from 'zod'
import type { JsonObject } from '../../../npm/plugin.js'
import type { Caller } from './engine.ts'

export const unloadingBeforeCall = 'tool provider generation is unloading; this call was not made. The tool is offered again once its provider is active; a later request uses the current provider.'
export const unloadingAfterCall = "tool provider generation is unloading; this call may already have taken effect. Check the operation's state before repeating it with the current provider."

const id = z.string().min(1)
const source = z.enum(['user', 'project', 'plugin'])
const start = {
  name: z.string().regex(/^[a-z0-9-]{1,64}$/), source: source.optional(),
  params: z.record(z.string(), z.string()).default({}),
}
export const toolRequest = z.discriminatedUnion('action', [
  z.strictObject({ action: z.literal('list') }),
  z.strictObject({ action: z.literal('start'), ...start }),
  z.strictObject({ action: z.literal('status'), run_id: id }),
  z.strictObject({ action: z.literal('stop'), run_id: id }),
])
export const management = {
  list: z.strictObject({ cwd: id }),
  // A user save writes the one document Settings holds, so it names the
  // revision its caller read as its CAS when the caller has one; a project
  // save replaces one file and takes its workspace instead.
  save: z.discriminatedUnion('source', [
    z.strictObject({ source: z.literal('user'), expectedRevision: z.string().min(1).optional(), document: z.unknown() }),
    z.strictObject({ source: z.literal('project'), cwd: id, document: z.unknown() }),
  ]),
  start: z.strictObject({ ...start, cwd: id, report_to: z.strictObject({ session_id: id, branch_id: id }).optional() }),
  status: z.strictObject({ run_id: id.optional() }),
  stop: z.strictObject({ run_id: id }),
}
export type SaveRequest = z.infer<typeof management.save>
export type RunRequest = z.infer<typeof toolRequest> & { caller: Caller }
export type RunReply = { content: string; structured?: JsonObject } | { code: string; message: string }
export interface WorkflowRuns { call(request: RunRequest): Promise<RunReply> }
