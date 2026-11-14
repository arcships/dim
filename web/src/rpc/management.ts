import { ProtocolError } from "./read"

type Json = Record<string, unknown>
export type AccountIdentity = { issuer: string; subject: string; email?: string; name?: string }
export type AccountState = { authenticated: boolean; recoveryRequired: boolean; identity?: AccountIdentity; preview?: string }
export type StaticAuth = { type: string; headerName?: string; prefix?: string }
export type ModelIdentification = {
  rule: string; narrowedByDisplayName: boolean
  candidates: { providerId: string; modelId: string }[]
  filled: { field: string; value: number; providerId: string; modelId: string }[]
}
export type ModelEntry = {
  modelId: string; displayName: string; status: string
  capabilities: { modalities: string[]; toolCalling: boolean; streaming: boolean; structuredOutput: boolean; vision: boolean; audio: boolean; reasoning: boolean; contextWindow?: number; maxOutputTokens?: number }
  metadata?: { reasoning?: { supported: boolean; effortOptions?: string[] }; serviceTier?: { supported: boolean; options: string[] }; identification?: ModelIdentification; matchedProviderId?: string; matchedModelId?: string }
}
export type ManagementModel = { model: ModelEntry; source: string; enabled: boolean; endpoint: string | null; endpointOverride?: string; alias?: string; unavailableReason: string | null; missingLimits: string[]; actions: string[] }
export type ManagementProvider = {
  providerId: string; displayName: string; source: string; credentialAuthority: string; version: number | null; enabled: boolean | null; actions: string[]
  account?: { namespace: string; loginMethods: string[]; state: AccountState }
  connection: { driverKind: string; auth: StaticAuth; baseUrl: string; credentialConfigured: boolean; headerNames: string[]; proxyUrl: string | null; proxyAuthConfigured: boolean; timeoutMs: number | null; gluedPrefix?: string } | null
  models: ManagementModel[] | null
  preferences: Record<string, { reasoningEffort?: string; serviceTier?: string }>
}
export type ManagementView = { drivers: { driverKind: string; authTypes: string[]; remoteModelList: boolean }[]; providers: ManagementProvider[]; errors: { providerId: string; message: string }[] }
export type LoginAttempt = { attemptId: string; expiresAt: number; authorizationUrl?: string; verificationUri?: string; verificationUriComplete?: string; userCode?: string }
export type AccountUsage = {
  subscriptionUrl?: string; pricingUrl?: string; subscriptionId?: number
  resets?: { window: { availableCount: number; nearestExpiresAt?: string }; monthlyFull: { availableCount: number; nearestExpiresAt?: string } }
  plan?: { name?: string; status?: string; priceAmount?: number; priceCurrency?: string; priceInterval?: string; expiresAt?: string; cancelAtPeriodEnd?: boolean; currentTermStart?: string; lastTermStatus?: string; lastTermStart?: string; lastTermEnd?: string }
  credits?: { usedUnits?: number; totalUnits?: number; remainingUnits?: number; unlimited?: boolean; resetsAt?: string }
  windows: { durationHours: number; tokenUsed: number; tokenCap: number; expiresAt?: string }[]
  features: { key: string; used?: number; allowance?: number; remaining?: number; unit?: string; unlimited?: boolean; periodEnd?: string }[]
}
export type AccountReferral = { enabled: boolean; eligible: boolean; affCode?: string; inviteUrl?: string; invitedCount: number; paidCount: number; rewardGrantedUnits?: number; inviterRebateBp: number; inviteeRebateBp: number }
export type ResetBatch = { grantId: number; remaining: number; expiresAt: string; scopeType: string; subscriptionId?: number; sourceType: string }

function object(value: unknown): Json {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new ProtocolError("Invalid management reply: expected object")
  return value as Json
}
function string(value: unknown): string {
  if (typeof value !== "string") throw new ProtocolError("Invalid management reply: expected string")
  return value
}
function number(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) throw new ProtocolError("Invalid management reply: expected nonnegative number")
  return value
}
function boolean(value: unknown): boolean {
  if (typeof value !== "boolean") throw new ProtocolError("Invalid management reply: expected boolean")
  return value
}
function list<T>(value: unknown, read: (item: unknown) => T): T[] {
  if (!Array.isArray(value)) throw new ProtocolError("Invalid management reply: expected array")
  return value.map(read)
}
function optional<T>(value: unknown, read: (item: unknown) => T): T | undefined { return value === undefined || value === null ? undefined : read(value) }
function httpUrl(value: unknown): string {
  const result = string(value)
  if (!["http:", "https:"].includes(new URL(result).protocol)) throw new ProtocolError("Invalid management URL")
  return result
}
export function readAccountDescriptors(value: unknown): { providerId: string; defaultDirectory?: string }[] {
  return list(value, (item) => { const p = object(item); return { providerId: string(p.providerId), defaultDirectory: optional(p.defaultDirectory, httpUrl) } })
}

export function readAccount(value: unknown): AccountState {
  const v = object(value)
  if ("authenticated" in v) return { authenticated: boolean(v.authenticated), recoveryRequired: boolean(v.recoveryRequired), preview: optional(v.accountIdPreview, string) }
  const identity = optional(v.account, (item) => {
    const a = object(item)
    return { issuer: string(a.issuer), subject: string(a.subject), email: optional(a.email, string), name: optional(a.name, string) }
  })
  return { authenticated: identity !== undefined, recoveryRequired: false, identity }
}
export function readModelEntry(value: unknown): ModelEntry {
  const v = object(value), c = object(v.capabilities)
  const metadata = optional(v.metadata, (item) => {
    const m = object(item)
    return {
      reasoning: optional(m.reasoning, (item) => { const r = object(item); return { supported: boolean(r.supported), effortOptions: optional(r.effortOptions, (v) => list(v, string)) } }),
      serviceTier: optional(m.serviceTier, (item) => { const s = object(item); return { supported: boolean(s.supported), options: s.options === undefined ? [] : list(s.options, string) } }),
      matchedProviderId: optional(m.matchedProviderId, string),
      matchedModelId: optional(m.matchedModelId, string),
      identification: optional(m.identification, (item) => {
        const i = object(item)
        return {
          rule: string(i.rule), narrowedByDisplayName: boolean(i.narrowedByDisplayName),
          candidates: i.candidates === undefined ? [] : list(i.candidates, (item) => { const c = object(item); return { providerId: string(c.providerId), modelId: string(c.modelId) } }),
          filled: i.filled === undefined ? [] : list(i.filled, (item) => { const f = object(item); return { field: string(f.field), value: number(f.value), providerId: string(f.providerId), modelId: string(f.modelId) } }),
        }
      }),
    }
  })
  return { modelId: string(v.modelId), displayName: string(v.displayName), status: string(v.status), capabilities: {
    modalities: list(c.modalities, string), toolCalling: boolean(c.toolCalling), streaming: boolean(c.streaming), structuredOutput: boolean(c.structuredOutput), vision: boolean(c.vision), audio: boolean(c.audio), reasoning: boolean(c.reasoning), contextWindow: optional(c.contextWindow, number), maxOutputTokens: optional(c.maxOutputTokens, number),
  }, metadata }
}
export function readManagementProvider(value: unknown): ManagementProvider {
  const v = object(value), preferences = object(v.preferences)
  return {
    providerId: string(v.providerId), displayName: string(v.displayName), source: string(v.source), credentialAuthority: string(v.credentialAuthority), version: v.version === null ? null : number(v.version), enabled: v.enabled === null ? null : boolean(v.enabled), actions: list(v.actions, string),
    account: optional(v.account, (item) => { const a = object(item); return { namespace: string(a.namespace), loginMethods: list(a.loginMethods, string), state: readAccount(a.state) } }),
    connection: v.connection === null ? null : (() => {
      const c = object(v.connection), auth = object(c.auth)
      return { driverKind: string(c.driverKind), auth: { type: string(auth.type), headerName: optional(auth.headerName, string), prefix: optional(auth.prefix, string) }, baseUrl: string(c.baseUrl), credentialConfigured: boolean(c.credentialConfigured), headerNames: list(c.headerNames, string), proxyUrl: c.proxyUrl === null ? null : string(c.proxyUrl), proxyAuthConfigured: boolean(c.proxyAuthConfigured), timeoutMs: c.timeoutMs === null ? null : number(c.timeoutMs), gluedPrefix: optional(c.gluedPrefix, string) }
    })(),
    models: v.models === null ? null : list(v.models, (item) => { const m = object(item); return { model: readModelEntry(m.model), source: string(m.source), enabled: boolean(m.enabled), endpoint: m.endpoint === null ? null : string(m.endpoint), endpointOverride: optional(m.endpointOverride, string), alias: optional(m.alias, string), unavailableReason: m.unavailableReason === null ? null : string(m.unavailableReason), missingLimits: m.missingLimits === undefined ? [] : list(m.missingLimits, string), actions: list(m.actions, string) } }),
    preferences: Object.fromEntries(Object.entries(preferences).map(([id, value]) => { const p = object(value); return [id, { reasoningEffort: optional(p.reasoningEffort, string), serviceTier: optional(p.serviceTier, string) }] })),
  }
}
export function readManagement(value: unknown): ManagementView {
  const v = object(value)
  return {
    drivers: list(v.drivers, (item) => { const d = object(item); return { driverKind: string(d.driverKind), authTypes: list(d.authTypes, string), remoteModelList: boolean(d.remoteModelList) } }),
    providers: list(v.providers, readManagementProvider),
    errors: list(v.errors, (item) => { const e = object(item); return { providerId: string(e.providerId), message: string(e.message) } }),
  }
}
export function readProbe(value: unknown): ModelEntry[] { return list(object(value).models, readModelEntry) }
export function readLoginAttempt(value: unknown): LoginAttempt {
  const v = object(value)
  return { attemptId: string(v.attemptId), expiresAt: number(v.expiresAt), authorizationUrl: optional(v.authorizationUrl, httpUrl), verificationUri: optional(v.verificationUri, httpUrl), verificationUriComplete: optional(v.verificationUriComplete, httpUrl), userCode: optional(v.userCode, string) }
}
export function readLoginStatus(value: unknown): string {
  const status = string(object(value).status)
  if (!["pending", "completed", "failed", "cancelled", "expired"].includes(status)) throw new ProtocolError("Invalid login status")
  return status
}
export function readUsage(value: unknown): AccountUsage {
  const v = object(value)
  return {
    subscriptionUrl: optional(v.subscriptionUrl, httpUrl), pricingUrl: optional(v.pricingUrl, httpUrl), subscriptionId: optional(v.subscriptionId, number),
    resets: optional(v.resets, (item) => { const r = object(item); const readKind = (item: unknown) => { const k = object(item); return { availableCount: number(k.availableCount), nearestExpiresAt: optional(k.nearestExpiresAt, string) } }; return { window: readKind(r.window), monthlyFull: readKind(r.monthlyFull) } }),
    plan: optional(v.plan, (item) => { const p = object(item); return { name: optional(p.name, string), status: optional(p.status, string), priceAmount: optional(p.priceAmount, number), priceCurrency: optional(p.priceCurrency, string), priceInterval: optional(p.priceInterval, string), expiresAt: optional(p.expiresAt, string), cancelAtPeriodEnd: optional(p.cancelAtPeriodEnd, boolean), currentTermStart: optional(p.currentTermStart, string), lastTermStatus: optional(p.lastTermStatus, string), lastTermStart: optional(p.lastTermStart, string), lastTermEnd: optional(p.lastTermEnd, string) } }),
    credits: optional(v.credits, (item) => { const c = object(item); return { usedUnits: optional(c.usedUnits, number), totalUnits: optional(c.totalUnits, number), remainingUnits: optional(c.remainingUnits, number), unlimited: optional(c.unlimited, boolean), resetsAt: optional(c.resetsAt, string) } }),
    windows: list(v.windows, (item) => { const w = object(item); return { durationHours: number(w.durationHours), tokenUsed: number(w.tokenUsed), tokenCap: number(w.tokenCap), expiresAt: optional(w.expiresAt, string) } }),
    features: list(v.features, (item) => { const f = object(item); return { key: string(f.key), used: optional(f.used, number), allowance: optional(f.allowance, number), remaining: optional(f.remaining, number), unit: optional(f.unit, string), unlimited: optional(f.unlimited, boolean), periodEnd: optional(f.periodEnd, string) } }),
  }
}
export function readReferral(value: unknown): AccountReferral {
  const v = object(value)
  return { enabled: boolean(v.enabled), eligible: boolean(v.eligible), affCode: optional(v.affCode, string), inviteUrl: optional(v.inviteUrl, httpUrl), invitedCount: number(v.invitedCount), paidCount: number(v.paidCount), rewardGrantedUnits: optional(v.rewardGrantedUnits, number), inviterRebateBp: number(v.inviterRebateBp), inviteeRebateBp: number(v.inviteeRebateBp) }
}
export function readResetBatches(value: unknown): { window: ResetBatch[]; monthlyFull: ResetBatch[] } {
  const v = object(value)
  const read = (item: unknown): ResetBatch => { const b = object(item); return { grantId: number(b.grantId), remaining: number(b.remaining), expiresAt: string(b.expiresAt), scopeType: string(b.scopeType), subscriptionId: optional(b.subscriptionId, number), sourceType: string(b.sourceType) } }
  return { window: list(v.window, read), monthlyFull: list(v.monthlyFull, read) }
}
