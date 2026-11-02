import { ProtocolError, readSummaries } from "./read"
import type { ModelId, SessionSummary } from "./protocol"

type ObjectValue = Record<string, unknown>

function object(value: unknown): ObjectValue {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new ProtocolError("Invalid settings reply: expected object")
  return value as ObjectValue
}
function string(value: unknown): string {
  if (typeof value !== "string") throw new ProtocolError("Invalid settings reply: expected string")
  return value
}
function boolean(value: unknown): boolean {
  if (typeof value !== "boolean") throw new ProtocolError("Invalid settings reply: expected boolean")
  return value
}
function number(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) throw new ProtocolError("Invalid settings reply: expected nonnegative integer")
  return value
}
function array<T>(value: unknown, read: (item: unknown) => T): T[] {
  if (!Array.isArray(value)) throw new ProtocolError("Invalid settings reply: expected array")
  return value.map(read)
}
function optionalString(value: unknown): string | undefined {
  return value === undefined || value === null ? undefined : string(value)
}
function choice<const T extends readonly string[]>(value: unknown, choices: T): T[number] {
  const text = string(value)
  if (!choices.includes(text)) throw new ProtocolError("Invalid settings reply: unknown option")
  return text as T[number]
}

export interface DefaultSelection extends ModelId { version: number }
export function readDefaultSelection(value: unknown): DefaultSelection | undefined {
  const source = object(value)
  return source.selection === null ? undefined : readSelectedModel(source.selection)
}
export function readSelectedModel(value: unknown): DefaultSelection {
  const source = object(value)
  return { providerId: string(source.provider_id), modelId: string(source.model_id), version: number(source.version) }
}

export type ModelPolicy = { mode: "agent" | "inherit" } | { mode: "fixed"; model: ModelId }
export type ModelPolicyView = { revision: string; policy: ModelPolicy }
export function readModelPolicy(value: unknown): ModelPolicyView {
  const source = object(value)
  const policy = object(source.policy)
  const mode = choice(policy.mode, ["agent", "inherit", "fixed"])
  if (mode !== "fixed") return { revision: string(source.revision), policy: { mode } }
  const model = object(policy.model)
  return { revision: string(source.revision), policy: { mode, model: { providerId: string(model.providerId), modelId: string(model.modelId) } } }
}

export type ExecShellView = { revision: string; executablePath: string | undefined; configError: string | undefined; shells: { executablePath: string; name: string }[] }
export function readExecShell(value: unknown): ExecShellView {
  const source = object(value)
  return { revision: string(source.revision), executablePath: optionalString(source.executablePath), configError: optionalString(source.configError), shells: array(source.shells, (item) => {
    const shell = object(item)
    return { executablePath: string(shell.executablePath), name: string(shell.name) }
  }) }
}

export type CommandRule = { command: string; match: "exact" | "prefix"; decision: "allow" | "deny" }
export type RulesView = { revision: string; commandRules: CommandRule[] }
export function readRules(value: unknown): RulesView {
  const source = object(value)
  return { revision: string(source.revision), commandRules: array(source.commandRules, (item) => {
    const rule = object(item)
    return { command: string(rule.command), match: choice(rule.match, ["exact", "prefix"]), decision: choice(rule.decision, ["allow", "deny"]) }
  }) }
}
export function readRevision(value: unknown): string { return string(object(value).revision) }

export type SkillEntry = { id: string; name: string; description: string; filePath: string; allowedTools: string[]; enabled: boolean; shadowedBy: string | undefined }
export type SkillIssue = { path: string; message: string }
export type SkillsView = { version: number; entries: SkillEntry[]; issues: SkillIssue[] }
export function readSkills(value: unknown): SkillsView {
  const source = object(value)
  return { version: number(source.version), entries: array(source.entries, (item) => {
    const skill = object(item)
    return { id: string(skill.id), name: string(skill.name), description: string(skill.description), filePath: string(skill.filePath), allowedTools: array(skill.allowedTools, string), enabled: boolean(skill.enabled), shadowedBy: optionalString(skill.shadowedBy) }
  }), issues: array(source.issues, (item) => {
    const issue = object(item)
    return { path: string(issue.path), message: string(issue.message) }
  }) }
}

export type PluginEntry = {
  name: string; use: string | undefined; off: boolean; locked: boolean; status: string | undefined; error: string | undefined
  candidates: { id: string; kinds: string[]; git: string | undefined }[]
  refused: { kind: string; error: string }[]
}
export function readPlugins(value: unknown): PluginEntry[] {
  return array(value, (item) => {
    const plugin = object(item)
    return {
      name: string(plugin.name), use: optionalString(plugin.use), off: boolean(plugin.off), locked: boolean(plugin.locked), status: optionalString(plugin.status), error: optionalString(plugin.error),
      candidates: array(plugin.candidates, (item) => {
        const candidate = object(item)
        const source = candidate.source === undefined ? undefined : object(candidate.source)
        return { id: string(candidate.id), kinds: array(candidate.kinds, string), git: source === undefined ? undefined : optionalString(source.git) }
      }),
      refused: plugin.refused === undefined ? [] : array(plugin.refused, (item) => {
        const refusal = object(item)
        return { kind: string(refusal.kind), error: string(refusal.error) }
      }),
    }
  })
}

export type McpEntry = { id: string; source: "user" | "project" | "plugin"; enabled: boolean; transport: string; auth: string | undefined }
export type McpView = { userRevision: string; projectRevision: string | undefined; servers: McpEntry[]; errors: string[] }
export function readMcp(value: unknown): McpView {
  const source = object(value)
  return {
    userRevision: string(source.userRevision), projectRevision: optionalString(source.projectRevision),
    servers: array(source.servers, (item) => {
      const server = object(item)
      return { id: string(server.id), source: choice(server.source, ["user", "project", "plugin"]), enabled: boolean(server.enabled), transport: string(server.transport), auth: server.auth === undefined ? undefined : string(object(server.auth).status) }
    }),
    errors: [source.userApplication, source.pluginApplication, source.projectApplication].flatMap((item) => {
      if (item === null) return []
      const state = object(item)
      return [optionalString(state.error), optionalString(state.cleanupError)].filter((error): error is string => error !== undefined)
    }),
  }
}

export type McpOAuthConfiguration = { clientId: string | undefined; callbackPort: number | undefined; scopes: string | undefined; metadataConfigured: boolean }
export type McpOAuthInput = { clientId?: string; callbackPort?: number; scopes?: string; authServerMetadataUrl?: string }
export type McpConfigurationPatch = {
  command?: string; args?: string[]; cwd?: string; url?: string
  env?: Record<string, string | null>; headers?: Record<string, string | null>
  bearer?: { action: "set"; token: string } | { action: "remove" }
  oauth?: { action: "remove" } | { action: "set"; clientId?: string | null; callbackPort?: number | null; scopes?: string | null; authServerMetadataUrl?: string | null }
}
export type McpServerInput =
  | { type: "stdio"; command: string; args: string[]; cwd?: string; env: Record<string, string>; enabled: boolean }
  | { type: "http" | "sse"; url: string; headers: Record<string, string>; enabled: boolean; auth?: { type: "bearer"; token: string }; oauth?: McpOAuthInput }
export type McpConfiguration =
  | { type: "stdio"; command: string; argsCount: number; cwd: string | undefined; envNames: string[] }
  | { type: "http" | "sse"; endpoint: string; headerNames: string[]; authConfigured: boolean; oauthConfigured: boolean; oauth: McpOAuthConfiguration | undefined }
export function readMcpConfiguration(value: unknown): McpConfiguration {
  const source = object(value)
  const type = choice(source.type, ["stdio", "http", "sse"])
  if (type === "stdio") return { type, command: string(source.command), argsCount: number(source.argsCount), cwd: optionalString(source.cwd), envNames: array(source.envNames, string) }
  const oauth = source.oauth === null ? undefined : object(source.oauth)
  return { type, endpoint: string(source.endpoint), headerNames: array(source.headerNames, string), authConfigured: boolean(source.authConfigured), oauthConfigured: boolean(source.oauthConfigured), oauth: oauth === undefined ? undefined : { clientId: optionalString(oauth.clientId), callbackPort: oauth.callbackPort === null ? undefined : number(oauth.callbackPort), scopes: optionalString(oauth.scopes), metadataConfigured: boolean(oauth.metadataConfigured) } }
}
export type McpEditableEntry = { revision: string | undefined; enabled: boolean; configuration: McpConfiguration }
export function readMcpEditableEntry(value: unknown): McpEditableEntry {
  const source = object(value)
  return { revision: optionalString(source.revision), enabled: boolean(source.enabled), configuration: readMcpConfiguration(source.configuration) }
}
export type McpTool = { name: string; description: string | undefined }
function readMcpTool(value: unknown): McpTool {
  const source = object(value)
  return { name: string(source.name), description: optionalString(source.description) }
}
export type McpInspection = { enabled: boolean; configuration: McpConfiguration; connections: { state: string; expired: boolean | undefined; tools: McpTool[] | undefined }[] }
export function readMcpInspection(value: unknown): McpInspection {
  const source = object(value)
  return { enabled: boolean(source.enabled), configuration: readMcpConfiguration(source.configuration), connections: array(source.connections, (value) => {
    const connection = object(value)
    const state = object(connection.state)
    const status = choice(state.status, ["disconnected", "connecting", "connected", "failed", "closed", "auth_required"])
    return { state: status, expired: status === "auth_required" ? boolean(state.expired) : undefined, tools: connection.tools === null ? undefined : array(connection.tools, readMcpTool) }
  }) }
}
export type McpProbe = {
  status: "connected" | "authRequired" | "failed"; latencyMs: number
  tools: McpTool[] | undefined; expired: boolean | undefined; missingScopes: string[] | undefined
  reason: string | undefined; message: string | undefined
}
export function readMcpTest(value: unknown): McpProbe {
  const source = object(value)
  return {
    status: choice(source.status, ["connected", "authRequired", "failed"]),
    latencyMs: number(source.latencyMs),
    tools: source.tools === null ? undefined : array(source.tools, readMcpTool),
    expired: source.expired === undefined ? undefined : boolean(source.expired),
    missingScopes: source.missingScopes === undefined ? undefined : array(source.missingScopes, string),
    reason: optionalString(source.reason), message: optionalString(source.message),
  }
}
export function readMcpAuthStart(value: unknown): { attemptId: string; authorizationUrl: string; redirectUri: string; expiresAt: number } {
  const source = object(value)
  const authorizationUrl = string(source.authorizationUrl)
  const url = new URL(authorizationUrl)
  if (!["https:", "http:"].includes(url.protocol) || url.username || url.password) throw new ProtocolError("Invalid MCP authorization URL")
  return { attemptId: string(source.attemptId), authorizationUrl, redirectUri: string(source.redirectUri), expiresAt: number(source.expiresAt) }
}

export type SavedSessions = { sessions: SessionSummary[]; nextCursor: string | undefined }
export function readSavedSessions(value: unknown): SavedSessions {
  const source = object(value)
  return { sessions: readSummaries(source.sessions), nextCursor: optionalString(source.nextCursor) }
}
export function readPing(value: unknown): { sessions: number; connections: number; serverVersion: string } {
  const source = object(value)
  return { sessions: number(source.sessions), connections: number(source.connections), serverVersion: string(source.serverVersion) }
}

export type ModelRecording = { revision: string; enabled: boolean }
export function readModelRecording(value: unknown): ModelRecording {
  const source = object(value)
  return { revision: string(source.revision), enabled: boolean(object(source.document).enabled) }
}

export type ExecutionSettings = { revision: string; sandbox: boolean; network: boolean }
export function readExecutionSettings(value: unknown): ExecutionSettings {
  const source = object(value)
  const document = object(source.document)
  return { revision: string(source.revision), sandbox: boolean(document.sandbox), network: boolean(document.network) }
}

export type SandboxBackend = { available: true } | { available: false; reason: string }
export function readSandboxBackend(value: unknown): SandboxBackend {
  const source = object(value)
  return boolean(source.available) ? { available: true } : { available: false, reason: string(source.reason) }
}

export type ModalityReason = { dimension: string; detail: string }
export type ModalityOption = ModelId & { available: boolean; reason?: ModalityReason }
export type ModalityEffective = ModelId & { source: "default" | "automatic" }
export type ModalityOperation = {
  operation: string
  key?: string
  available: boolean
  reasons: ModalityReason[]
  default?: ModalityOption
  effective?: ModalityEffective
  options: ModalityOption[]
}
export type ModalityCategory = { category: string; available: boolean; reasons: ModalityReason[]; operations: ModalityOperation[] }
export type ModalitySnapshot = { revision: string; categories: ModalityCategory[] }
function readModalityReason(value: unknown): ModalityReason {
  const source = object(value)
  return { dimension: string(source.dimension), detail: string(source.detail) }
}
function readModalityOption(value: unknown): ModalityOption {
  const source = object(value)
  const reason = source.reason === null || source.reason === undefined ? undefined : readModalityReason(source.reason)
  return { providerId: string(source.providerId), modelId: string(source.modelId), available: boolean(source.available), ...(reason === undefined ? {} : { reason }) }
}
function readModalityOperation(value: unknown): ModalityOperation {
  const source = object(value)
  const key = optionalString(source.key)
  const effective = source.effective === null || source.effective === undefined ? undefined : object(source.effective)
  return {
    operation: string(source.operation),
    ...(key === undefined ? {} : { key }),
    available: boolean(source.available),
    reasons: array(source.reasons, readModalityReason),
    ...(source.default === null || source.default === undefined ? {} : { default: readModalityOption(source.default) }),
    ...(effective === undefined ? {} : { effective: { providerId: string(effective.providerId), modelId: string(effective.modelId), source: choice(effective.source, ["default", "automatic"]) } }),
    options: array(source.options, readModalityOption),
  }
}
export function readModalitySnapshot(value: unknown): ModalitySnapshot {
  const source = object(value)
  return {
    revision: string(source.revision),
    categories: array(source.categories, (item) => {
      const category = object(item)
      return {
        category: string(category.category),
        available: boolean(category.available),
        reasons: array(category.reasons, readModalityReason),
        operations: array(category.operations, readModalityOperation),
      }
    }),
  }
}
