import { z } from 'zod'
import { parseDocument } from 'yaml'
import type { PermissionPolicy } from '../../../npm/plugin.js'

/** One definition's compact JSON; several share a user document and the run store. */
export const definitionBytes = 1024 * 1024
/** One Workflow payload crossing the bridge, in its serialized bytes: the user
 * document, the store text as a JSON string, or a list/status reply. Half of the
 * 16 MiB frame stays for the service envelope. */
export const payloadBytes = 8 * 1024 * 1024
/** A run's stored stage result texts in total, in the committed JSON's bytes. */
export const resultBytes = 512 * 1024
/** One stored run log message, in the committed JSON's bytes. */
export const logMessageBytes = 512
/** One stored stage reason, in the committed JSON's bytes. */
export const stageReasonBytes = 512
export const logEntries = 500

export class WorkflowError extends Error {
  readonly code: string
  constructor(code: string, message = code) {
    super(message)
    this.code = code
  }
}

const identity = z.string().regex(/^[a-z0-9-]{1,32}$/)
export const stageSchema = z.strictObject({
  id: identity,
  prompt: z.string(),
  agent: z.string().min(1).optional(),
  model: z.strictObject({ providerId: z.string().min(1), modelId: z.string().min(1) }).optional(),
})
export const definitionSchema = z.strictObject({
  schema: z.literal('dim.workflow/v1'),
  name: z.string().regex(/^[a-z0-9-]{1,64}$/),
  description: z.string(),
  policy: z.enum(['read_only', 'workspace_write', 'full_access']).optional(),
  parameters: z.array(z.strictObject({
    name: identity,
    description: z.string().optional(),
    required: z.boolean().optional(),
    default: z.string().optional(),
  })).optional(),
  steps: z.array(z.union([
    z.strictObject({ stage: stageSchema }),
    z.strictObject({ parallel: z.array(stageSchema).min(2).max(8) }),
  ])).min(1).max(32),
})
export type Definition = z.infer<typeof definitionSchema>
export type StageDefinition = z.infer<typeof stageSchema>
export type Source = 'user' | 'project' | 'plugin'
export const settingsSchema = z.strictObject({
  definitions: z.record(z.string(), definitionSchema).default({}),
  projectEnabled: z.record(z.string(), z.record(z.string(), z.boolean())).default({}),
  max_concurrent_runs: z.number().int().min(1).default(4),
  max_concurrent_stages: z.number().int().min(1).default(4),
})
export type WorkflowSettings = z.infer<typeof settingsSchema>

export function stages(step: Definition['steps'][number]): StageDefinition[] {
  return 'stage' in step ? [step.stage] : step.parallel
}

function tokens(prompt: string, accept: (token: string) => string): string {
  return prompt.replace(/\{\{([\s\S]*?)(?:\}\}|$)/g, (match, token: string) => {
    if (!match.endsWith('}}')) throw new WorkflowError('invalid_definition', 'Unterminated template opener')
    return accept(token)
  })
}

/** Semantic rules apply to Settings-certified definitions without reparsing them. */
export function issues(definition: Definition, name: string): string[] {
  const found: string[] = []
  if (definition.name !== name) found.push(`Document name must match ${name}`)
  const parameters = new Set<string>()
  for (const parameter of definition.parameters ?? []) {
    if (parameters.has(parameter.name)) found.push(`Duplicate parameter ${parameter.name}`)
    parameters.add(parameter.name)
  }
  const all = new Set<string>()
  const previous = new Set<string>()
  for (const step of definition.steps) {
    for (const stage of stages(step)) {
      if (all.has(stage.id)) found.push(`Duplicate stage ${stage.id}`)
      all.add(stage.id)
      try {
        const literal = tokens(stage.prompt, token => {
          const parameter = /^params\.([a-z0-9-]{1,32})$/.exec(token)
          const result = /^stages\.([a-z0-9-]{1,32})\.result$/.exec(token)
          if (parameter && parameters.has(parameter[1])) return ''
          if (result && previous.has(result[1])) return ''
          found.push(`Stage ${stage.id}: invalid reference {{${token}}}`)
          return ''
        })
        if (Buffer.byteLength(literal) > 32 * 1024) found.push(`Stage ${stage.id}: literal prompt exceeds 32 KiB`)
      } catch (error) {
        if (!(error instanceof WorkflowError)) throw error
        found.push(`Stage ${stage.id}: ${error.message}`)
      }
    }
    for (const stage of stages(step)) previous.add(stage.id)
  }
  return found
}

/** Count compact JSON without materializing an alias-expanded document. */
export function withinDefinitionLimit(definition: Definition): boolean {
  let remaining = definitionBytes
  function visit(value: unknown): boolean {
    if (value !== null && typeof value === 'object') {
      remaining -= 2
      let comma = false
      for (const [key, item] of Object.entries(value)) {
        if (item === undefined) continue
        if (comma) remaining--
        if (!Array.isArray(value)) remaining -= Buffer.byteLength(JSON.stringify(key)) + 1
        if (remaining < 0 || !visit(item)) return false
        comma = true
      }
    } else remaining -= Buffer.byteLength(JSON.stringify(value))
    return remaining >= 0
  }
  return visit(definition)
}

/** This is the boundary for untrusted tool/management documents and YAML bytes. */
export function decodeDefinition(value: unknown): Definition {
  const decoded = definitionSchema.safeParse(value)
  if (!decoded.success) throw new WorkflowError('invalid_definition', decoded.error.message)
  if (!withinDefinitionLimit(decoded.data)) {
    throw new WorkflowError('invalid_definition', 'Definition exceeds 1 MiB')
  }
  return decoded.data
}

export function decodeFile(bytes: Uint8Array): Definition {
  try {
    const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes)
    const document = parseDocument(text, { uniqueKeys: true })
    if (document.errors.length) throw new Error(document.errors.map(error => error.message).join('; '))
    return decodeDefinition(document.toJS({ maxAliasCount: 100 }))
  } catch (error) {
    if (error instanceof WorkflowError) throw error
    throw new WorkflowError('invalid_definition', error instanceof Error ? error.message : 'Invalid UTF-8/YAML')
  }
}

export function parameters(definition: Definition, supplied: Record<string, string>): Record<string, string> {
  const accepted: Record<string, string> = {}
  let total = 0
  const declared = definition.parameters ?? []
  if (Object.keys(supplied).some(name => !declared.some(parameter => parameter.name === name))) {
    throw new WorkflowError('invalid_parameters', 'Unknown parameter')
  }
  for (const parameter of declared) {
    const value = Object.hasOwn(supplied, parameter.name) ? supplied[parameter.name] : parameter.default
    if (value === undefined && parameter.required) {
      throw new WorkflowError('invalid_parameters', `Missing parameter ${parameter.name}`)
    }
    const size = Buffer.byteLength(value ?? '')
    if (size > 32 * 1024) throw new WorkflowError('invalid_parameters', `Parameter ${parameter.name} exceeds 32 KiB`)
    total += size
    if (total > 256 * 1024) throw new WorkflowError('invalid_parameters', 'Parameters exceed 256 KiB in total')
    accepted[parameter.name] = value ?? ''
  }
  return accepted
}

export function render(stage: StageDefinition, params: Record<string, string>, results: Record<string, string>): string {
  const prompt = tokens(stage.prompt, token => {
    if (token.startsWith('params.')) return params[token.slice(7)]
    return results[token.slice(7, -7)]
  })
  if (Buffer.byteLength(prompt) > 32 * 1024) throw new WorkflowError('prompt_too_large')
  return prompt
}

export function policy(definition: Definition): PermissionPolicy | undefined {
  if (!definition.policy) return undefined
  return { preset: definition.policy.replaceAll('_', '-') as PermissionPolicy['preset'], allowedTools: [], disallowedTools: [] }
}
