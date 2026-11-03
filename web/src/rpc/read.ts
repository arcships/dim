/**
 * Strict readers for App Server replies and notifications: the one place the
 * web workspace parses wire data. A value that does not match its contract
 * throws a ProtocolError here; nothing past this file re-checks shapes.
 */
import type {
  ApprovalRequest,
  AskCall,
  AskQuestion,
  CompactResult,
  ContextUsage,
  FileCandidate,
  PermissionPolicy,
  PermissionPreset,
  PermissionState,
  MemberApprovalDetail,
  BranchCreateResult,
  BranchSource,
  CreateResult,
  EventContent,
  HelloResult,
  HistoryPage,
  ModelId,
  ModelUpdateResult,
  Notification,
  ProviderCatalog,
  ProviderSummary,
  RawBlock,
  SessionEvent,
  SessionSummary,
  ToolResult,
  UserInput,
  WithdrawResult,
} from "./protocol"

export class ProtocolError extends Error {
  override name = "ProtocolError"
}

type Json = Record<string, unknown>

function fail(at: string, expected: string): never {
  throw new ProtocolError(`Invalid ${at}: expected ${expected}`)
}

function obj(value: unknown, at: string): Json {
  if (value === null || typeof value !== "object" || Array.isArray(value)) fail(at, "an object")
  return value as Json
}

function arr(value: unknown, at: string): unknown[] {
  if (!Array.isArray(value)) fail(at, "an array")
  return value
}

function str(source: Json, key: string, at: string): string {
  const value = source[key]
  if (typeof value !== "string") fail(`${at}.${key}`, "a string")
  return value
}

function optStr(source: Json, key: string, at: string): string | undefined {
  return source[key] === undefined || source[key] === null ? undefined : str(source, key, at)
}

function num(source: Json, key: string, at: string): number {
  const value = source[key]
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) fail(`${at}.${key}`, "a nonnegative safe integer")
  return value
}

/** A nonnegative finite number; token counts are floats on the wire. */
function finite(source: Json, key: string, at: string): number {
  const value = source[key]
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) fail(`${at}.${key}`, "a nonnegative number")
  return value
}

function bool(source: Json, key: string, at: string): boolean {
  const value = source[key]
  if (typeof value !== "boolean") fail(`${at}.${key}`, "a boolean")
  return value
}

function modelId(value: unknown, at: string): ModelId {
  const source = obj(value, at)
  return { providerId: str(source, "providerId", at), modelId: str(source, "modelId", at) }
}


export function readHello(value: unknown): HelloResult {
  const source = obj(value, "hello")
  return {
    instanceId: str(source, "instanceId", "hello"),
    serverVersion: str(source, "serverVersion", "hello"),
    protocolVersion: num(source, "protocolVersion", "hello"),
    features: arr(source.features, "hello.features").map((feature, index) => {
      if (typeof feature !== "string") fail(`hello.features[${index}]`, "a string")
      return feature
    }),
  }
}

/** A reply whose content the caller does not use (`{}`, a barrier, the submitted event). */
export function readAck(value: unknown): void {
  obj(value, "reply")
}

/** `session.methods` of one Session: the method names it serves. */
export function readSessionMethods(value: unknown): string[] {
  const at = "session.methods"
  return arr(obj(value, at).methods, `${at}.methods`).map((item, index) => str(obj(item, `${at}.methods[${index}]`), "name", `${at}.methods[${index}]`))
}

export function readCompactResult(value: unknown): CompactResult {
  const at = "session.context.compact"
  const source = obj(value, at)
  const status = str(source, "status", at)
  if (status === "completed") return { status }
  if (status === "skipped") return { status, reason: str(source, "reason", at) }
  fail(`${at}.status`, "completed or skipped")
}

export function readContextUsage(value: unknown): ContextUsage {
  const at = "session.context.usage"
  const source = obj(value, at)
  return {
    contextWindow: finite(source, "contextWindow", at),
    outputTokens: finite(source, "outputTokens", at),
    thresholdTokens: finite(source, "thresholdTokens", at),
    estimatedInputTokens: finite(source, "estimatedInputTokens", at),
    baselineSource: str(source, "baselineSource", at),
  }
}

/** `session.fork`: the new Session's id. */
export function readForkedSessionId(value: unknown): string {
  return str(obj(value, "session.fork"), "sessionId", "session.fork")
}

export function readSummaries(value: unknown): SessionSummary[] {
  return arr(value, "session.summaries").map((item, index) => {
    const at = `session.summaries[${index}]`
    const source = obj(item, at)
    const preview = optStr(source, "preview", at)
    return {
      sessionId: str(source, "sessionId", at),
      cwd: str(source, "cwd", at),
      model: modelId(source.model, `${at}.model`),
      createdAt: num(source, "createdAt", at),
      running: bool(source, "running", at),
      sequence: num(source, "sequence", at),
      ...(preview === undefined ? {} : { preview }),
      updatedAt: num(source, "updatedAt", at),
    }
  })
}

export function readCreate(value: unknown): CreateResult {
  const source = obj(value, "session.create")
  return {
    sessionId: str(source, "sessionId", "session.create"),
    model: modelId(source.model, "session.create.model"),
    sequence: num(source, "sequence", "session.create"),
  }
}

export function readBranchCreate(value: unknown): BranchCreateResult {
  const source = obj(value, "session.branch_create")
  const branch = obj(source.branch, "session.branch_create.branch")
  const at = "session.branch_create.branch"
  return {
    branchId: str(branch, "branchId", at),
    ...(branch.parentBranchId === undefined || branch.parentBranchId === null ? {} : { parentBranchId: str(branch, "parentBranchId", at) }),
    ...(branch.branchPoint === undefined || branch.branchPoint === null ? {} : { branchPoint: num(branch, "branchPoint", at) }),
    ...(branch.createEvent === undefined || branch.createEvent === null ? {} : { createEvent: num(branch, "createEvent", at) }),
  }
}

export function readHistory(value: unknown): HistoryPage {
  const source = obj(value, "session.history")
  return {
    events: arr(source.events, "session.history.events").map((event, index) => readEvent(event, `session.history.events[${index}]`)),
    through: num(source, "through", "session.history"),
    nextAfter: num(source, "nextAfter", "session.history"),
    done: bool(source, "done", "session.history"),
  }
}

export function readModelUpdate(value: unknown): ModelUpdateResult {
  const source = obj(value, "session.model.update")
  return { model: modelId(source.model, "session.model.update.model"), outcome: str(source, "outcome", "session.model.update") }
}

/** A permission preset as the server names it; anything else is a protocol error. */
function readPreset(value: unknown, at: string): PermissionPreset {
  if (value === "read-only" || value === "workspace-write" || value === "full-access") return value
  return fail(at, "a permission preset")
}

function readPermissionPolicy(value: unknown, at: string): PermissionPolicy {
  const policy = obj(value, at)
  const list = (key: string) => arr(policy[key], `${at}.${key}`).map((item, index) => {
    if (typeof item !== "string") fail(`${at}.${key}[${index}]`, "a string")
    return item
  })
  return {
    preset: readPreset(policy.preset, `${at}.preset`),
    allowedTools: list("allowedTools"),
    disallowedTools: list("disallowedTools"),
  }
}

/** `session.permission.get`: the policy of `main` and the version a change must expect. */
export function readPermissionState(value: unknown): PermissionState {
  const at = "session.permission.get"
  const source = obj(value, at)
  return { version: num(source, "version", at), policy: readPermissionPolicy(source.policy, `${at}.policy`) }
}

/** `session.permission.update`: the version the new policy is stored under. */
export function readPermissionVersion(value: unknown): number {
  return num(obj(value, "session.permission.update"), "version", "session.permission.update")
}

/** `session.files.search`: the files an `@` mention offers, best first. */
export function readFileCandidates(value: unknown): FileCandidate[] {
  const at = "session.files.search"
  return arr(obj(value, at).candidates, `${at}.candidates`).map((item, index) => {
    const entry = `${at}.candidates[${index}]`
    const source = obj(item, entry)
    return { relativePath: str(source, "relativePath", entry), absolutePath: str(source, "absolutePath", entry), dir: bool(source, "dir", entry) }
  })
}

/** `attachment.put`: the stored text's path, which the message names. */
export function readAttachmentPath(value: unknown): string {
  return str(obj(value, "attachment.put"), "path", "attachment.put")
}

/** The `outcome` of a `session.model.update` that only changes preferences. */
export function readConfigurationOutcome(value: unknown): string {
  return str(obj(value, "session.model.update"), "outcome", "session.model.update")
}

export function readProviderList(value: unknown): ProviderSummary[] {
  const source = obj(value, "provider.list")
  return arr(source.providers, "provider.list.providers").map((item, index) => {
    const at = `provider.list.providers[${index}]`
    const provider = obj(item, at)
    return {
      providerId: str(provider, "providerId", at),
      displayName: str(provider, "displayName", at),
      enabled: bool(provider, "enabled", at),
      credentialConfigured: bool(provider, "credentialConfigured", at),
    }
  })
}

export function readAccountProviders(value: unknown): Pick<ProviderSummary, "providerId" | "displayName">[] {
  return arr(value, "account.providers").map((item, index) => {
    const at = `account.providers[${index}]`
    const provider = obj(item, at)
    return { providerId: str(provider, "providerId", at), displayName: str(provider, "displayName", at) }
  })
}

/** `provider.catalog`: `{catalog: null}` when the provider has no catalog. */
/**
 * The reasoning effort values a catalog model accepts. Undefined unless the
 * model advertises reasoning preferences, as `/effort` requires (`configure.rs`).
 */
function readReasoningEfforts(model: Json, at: string): string[] | undefined {
  const metadata = model.metadata === undefined || model.metadata === null ? undefined : obj(model.metadata, `${at}.metadata`)
  const reasoning = metadata?.reasoning === undefined || metadata.reasoning === null ? undefined : obj(metadata.reasoning, `${at}.metadata.reasoning`)
  if (reasoning === undefined || reasoning.supported !== true) return undefined
  const capabilities = obj(model.capabilities, `${at}.capabilities`)
  if (capabilities.reasoning !== true) return undefined
  const efforts = reasoning.effortOptions === undefined || reasoning.effortOptions === null
    ? []
    : arr(reasoning.effortOptions, `${at}.metadata.reasoning.effortOptions`).map((effort, index) => {
      if (typeof effort !== "string") fail(`${at}.metadata.reasoning.effortOptions[${index}]`, "a string")
      return effort
    })
  if (reasoning.off !== undefined && reasoning.off !== null && !efforts.includes("none")) efforts.push("none")
  return efforts
}

export function readProviderCatalog(value: unknown): ProviderCatalog | undefined {
  const source = obj(value, "provider.catalog")
  if (source.catalog === null) return undefined
  const catalog = obj(source.catalog, "provider.catalog.catalog")
  return {
    models: arr(catalog.models, "provider.catalog.models").map((item, index) => {
      const at = `provider.catalog.models[${index}]`
      const model = obj(item, at)
      const unavailableReason = optStr(model, "unavailableReason", at)
      const reasoningEfforts = readReasoningEfforts(model, at)
      return {
        id: str(model, "id", at),
        name: str(model, "name", at),
        ...(unavailableReason === undefined ? {} : { unavailableReason }),
        ...(reasoningEfforts === undefined ? {} : { reasoningEfforts }),
      }
    }),
    disabledModelIds: arr(catalog.disabledModelIds, "provider.catalog.disabledModelIds").map((id, index) => {
      if (typeof id !== "string") fail(`provider.catalog.disabledModelIds[${index}]`, "a string")
      return id
    }),
  }
}

/**
 * `provider.selection`: the default model for a directory. The selection
 * object serializes in snake_case (`ProviderSelection` has no rename).
 */
export function readProviderSelection(value: unknown): ModelId | undefined {
  const source = obj(value, "provider.selection")
  if (source.selection === null) return undefined
  const selection = obj(source.selection, "provider.selection.selection")
  return { providerId: str(selection, "provider_id", "provider.selection"), modelId: str(selection, "model_id", "provider.selection") }
}


/** A user message's content blocks, kept as the server wrote them so a steer can resend them unchanged. */
function readUserInput(user: Json, at: string): UserInput {
  return { content: arr(user.content, `${at}.content`).map((item, index) => obj(item, `${at}.content[${index}]`)) }
}

/** `session.input.withdraw`'s status; a steer adds a replacement input that arrives as its own event. */
export function readWithdrawResult(value: unknown): WithdrawResult {
  const at = "session.input.withdraw"
  const status = str(obj(value, at), "status", at)
  if (status === "deleted" || status === "notHeld") return { status }
  fail(`${at}.status`, "deleted or notHeld")
}

const SKILL_BLOCK = '<dim-skill name="'

/** A loaded Skill's block names it after the marker (Skills §9). */
function loadedSkill(text: string): string | undefined {
  if (!text.startsWith(SKILL_BLOCK)) return undefined
  const end = text.indexOf('"', SKILL_BLOCK.length)
  if (end < 0) return undefined
  return text.slice(SKILL_BLOCK.length, end)
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&quot;", '"')
    .replaceAll("&amp;", "&")
}

/**
 * The operator's own text of a message and the Skills its blocks loaded: a
 * text block after the first that starts with the marker is a loaded Skill,
 * shown as one line and never as its instructions.
 */
function userText(blocks: unknown, at: string): { text: string; skills: string[] } {
  const text: string[] = []
  const skills: string[] = []
  arr(blocks, at).forEach((item, index) => {
    const block = obj(item, `${at}[${index}]`)
    if (block.type !== "text" || typeof block.text !== "string") return
    const skill = index === 0 ? undefined : loadedSkill(block.text)
    if (skill === undefined) text.push(block.text)
    else skills.push(skill)
  })
  return { text: text.join("\n"), skills }
}

function textOf(blocks: unknown, at: string): string {
  return arr(blocks, at).flatMap((item, index) => {
    const block = obj(item, `${at}[${index}]`)
    return block.type === "text" ? [str(block, "text", `${at}[${index}]`)] : []
  }).join("\n")
}

function toolResult(value: unknown, at: string): ToolResult {
  const source = obj(value, at)
  const text = typeof source.content === "string" ? source.content : textOf(source.content, `${at}.content`)
  const structured = source.structuredContent === undefined || source.structuredContent === null ? undefined : obj(source.structuredContent, `${at}.structuredContent`)
  const diff = typeof structured?.diff === "string" ? structured.diff : undefined
  return { text, isError: source.isError === true, ...(diff === undefined ? {} : { diff }) }
}

function rawBlocks(blocks: unknown, at: string): RawBlock[] {
  if (blocks === undefined || blocks === null) return []
  return arr(blocks, at).flatMap((item, index) => {
    const block = obj(item, `${at}[${index}]`)
    if (block.type !== "raw") return []
    const key = optStr(block, "key", `${at}[${index}]`)
    return [{ text: str(block, "text", `${at}[${index}]`), ...(key === undefined ? {} : { key }), retire: block.retire === true }]
  })
}

function branchSource(value: unknown, at: string): BranchSource {
  const source = obj(value, at)
  const kind = str(source, "kind", at)
  if (kind === "tool") {
    return {
      kind,
      turnId: str(source, "turn_id", at),
      requestId: str(source, "request_id", at),
      callId: str(source, "call_id", at),
    }
  }
  if (kind === "host") return { kind, operationId: str(source, "operation_id", at) }
  return fail(at, "a branch source")
}

function branchCut(value: unknown, at: string): number | undefined {
  const source = obj(value, at)
  switch (str(source, "kind", at)) {
    case "sequence": return num(source, "sequence", at)
    case "create": return undefined
    default: return fail(at, "a branch cut")
  }
}

function content(type: string, name: string, data: Json, at: string): EventContent {
  switch (`${type}/${name}`) {
    case "input/message": {
      const blocks = rawBlocks(data.blocks, `${at}.blocks`)
      if (data.user === undefined || data.user === null) {
        if (blocks.length === 0) return { kind: "input", blocks }
        const mode = optStr(data, "mode", at)
        const text = blocks.map((block) => block.text).find((value) => value.startsWith("<dim-branch-")) ?? blocks[0]!.text
        return { kind: "source-input", text, blocks, ...(mode === undefined ? {} : { mode }) }
      }
      const user = obj(data.user, `${at}.user`)
      const { text, skills } = userText(user.content, `${at}.user.content`)
      return { kind: "input", text, user: readUserInput(user, `${at}.user`), skills: skills.length === 0 ? undefined : skills, blocks }
    }
    case "input/applied":
      return { kind: "input-applied", inputId: str(data, "input_id", at) }
    case "permission/changed":
      return { kind: "permission-changed", policy: readPermissionPolicy(data.policy, `${at}.policy`) }
    case "input/deleted":
      return { kind: "input-deleted", inputId: str(data, "input_id", at) }
    case "branch/start":
      return { kind: "branch-start" }
    case "session/forked":
      return { kind: "session-forked" }
    case "branch/create":
      return {
        kind: "branch-create",
        branchId: str(data, "branch", at),
        at: branchCut(data.at, `${at}.at`),
        prompt: str(data, "prompt", at),
        source: branchSource(data.source, `${at}.source`),
      }
    case "branch/create-failed": {
      const status = str(data, "status", at)
      if (status !== "failed" && status !== "cancelled") fail(`${at}.status`, "a creation outcome")
      return {
        kind: "branch-create-failed",
        branchId: str(data, "branch", at),
        status,
        error: str(data, status === "failed" ? "error" : "reason", at),
      }
    }
    case "branch/end":
      return {
        kind: "branch-end",
        source: branchSource(data.source, `${at}.source`),
        ...(data.ancestor === undefined || data.ancestor === null ? {} : { ancestor: str(data, "ancestor", at) }),
      }
    case "branch/ended":
      return {
        kind: "branch-ended",
        branchId: str(data, "branch", at),
        source: branchSource(data.source, `${at}.source`),
        ...(data.ancestor === undefined || data.ancestor === null ? {} : { ancestor: str(data, "ancestor", at) }),
      }
    case "turn/end": {
      const status = str(data, "status", at)
      if (status !== "completed" && status !== "failed" && status !== "cancelled") fail(`${at}.status`, "a turn outcome")
      const error = status === "failed" ? str(data, "error", at) : undefined
      return { kind: "turn-end", outcome: status, ...(error === undefined ? {} : { error }) }
    }
    case "step/model-start":
      return { kind: "model-start", requestId: str(data, "request_id", at) }
    case "step/thinking":
      return { kind: "thinking", requestId: str(data, "request_id", at), delta: str(data, "delta", at) }
    case "step/text":
      return { kind: "text", requestId: str(data, "request_id", at), delta: str(data, "delta", at) }
    case "step/tool-call":
      return { kind: "tool-call", requestId: str(data, "request_id", at), asks: readAsks(arr(data.calls, `${at}.calls`), `${at}.calls`) }
    case "step/tool-use": {
      const call = obj(data.call, `${at}.call`)
      return {
        kind: "tool-use",
        requestId: str(data, "request_id", at),
        callId: str(call, "id", `${at}.call`),
        name: str(call, "name", `${at}.call`),
        input: obj(call.input, `${at}.call.input`),
      }
    }
    case "step/tool-completed":
      return { kind: "tool-completed", callId: str(data, "call_id", at), result: toolResult(data.result, `${at}.result`) }
    case "step/tool-result":
      return {
        kind: "tool-result",
        requestId: str(data, "request_id", at),
        results: arr(data.results, `${at}.results`).map((item, index) => {
          const result = obj(item, `${at}.results[${index}]`)
          return { callId: str(result, "call_id", `${at}.results[${index}]`), result: toolResult(result.result, `${at}.results[${index}].result`) }
        }),
      }
    case "step/model-output-end": {
      const error = str(data, "status", at) === "failed" ? str(data, "error", at) : undefined
      return { kind: "model-end", requestId: str(data, "request_id", at), ...(error === undefined ? {} : { error }) }
    }
    case "context/configured":
      return { kind: "context-configured", model: { providerId: str(data, "provider_id", at), modelId: str(data, "model_id", at) } }
    default:
      return { kind: "other" }
  }
}

/**
 * The `ask_user` input as Questions A.1 checks it. `undefined` when the input
 * breaks a rule: the server rejects such a call at once, so it gets no prompt.
 */
function readAskQuestions(input: Json): AskQuestion[] | undefined {
  if (!Array.isArray(input.questions) || input.questions.length < 1 || input.questions.length > 4) return undefined
  const ids = new Set<string>()
  const questions: AskQuestion[] = []
  for (const [index, item] of input.questions.entries()) {
    const question = askQuestion(item, String(index + 1))
    if (question === undefined || ids.has(question.id)) return undefined
    ids.add(question.id)
    questions.push(question)
  }
  return questions
}

function askQuestion(value: unknown, defaultId: string): AskQuestion | undefined {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return undefined
  const source = value as Json
  const id = source.id ?? defaultId
  if (typeof id !== "string" || id === "") return undefined
  if (source.header !== undefined && typeof source.header !== "string") return undefined
  if (typeof source.header === "string" && [...source.header].length > 40) return undefined
  if (typeof source.question !== "string" || source.question.trim() === "") return undefined
  if (!Array.isArray(source.options) || source.options.length < 2 || source.options.length > 6) return undefined
  const labels = new Set<string>()
  const options: AskQuestion["options"] = []
  for (const item of source.options) {
    if (item === null || typeof item !== "object" || Array.isArray(item)) return undefined
    const option = item as Json
    if (typeof option.label !== "string" || option.label.trim() === "" || labels.has(option.label)) return undefined
    if (option.description !== undefined && typeof option.description !== "string") return undefined
    labels.add(option.label)
    options.push({ label: option.label, ...(typeof option.description === "string" ? { description: option.description } : {}) })
  }
  if ((source.multiSelect !== undefined && typeof source.multiSelect !== "boolean") || (source.allowCustom !== undefined && typeof source.allowCustom !== "boolean")) return undefined
  return {
    id,
    ...(typeof source.header === "string" ? { header: source.header } : {}),
    question: source.question,
    options,
    multiSelect: source.multiSelect === true,
    allowCustom: source.allowCustom !== false,
  }
}

function readAsks(calls: unknown[], at: string): AskCall[] {
  const asks: AskCall[] = []
  for (const [index, item] of calls.entries()) {
    const call = obj(item, `${at}[${index}]`)
    if (str(call, "name", `${at}[${index}]`) !== "ask_user") continue
    const questions = readAskQuestions(obj(call.input, `${at}[${index}].input`))
    if (questions !== undefined) asks.push({ callId: str(call, "id", `${at}[${index}]`), questions })
  }
  return asks
}

function readEvent(value: unknown, at: string): SessionEvent {
  const source = obj(value, at)
  const type = str(source, "type", at)
  const name = str(source, "name", at)
  return {
    id: str(source, "id", at),
    sequence: num(source, "sequence", at),
    createAt: num(source, "create_at", at),
    content: content(type, name, obj(source.data, `${at}.data`), `${at}.data`),
  }
}


function readApproval(params: Json): ApprovalRequest {
  const at = "approval.request"
  const call = obj(params.call, `${at}.call`)
  const source = obj(params.source, `${at}.source`)
  const callId = source.kind === "tool" ? str(source, "call_id", `${at}.source`) : undefined
  return {
    sessionId: str(params, "sessionId", at),
    branchId: str(params, "branchId", at),
    requestId: str(params, "requestId", at),
    ...(callId === undefined ? {} : { callId }),
    call: { name: str(call, "name", `${at}.call`), input: obj(call.input, `${at}.call.input`) },
    reason: str(params, "reason", at),
  }
}

export function readMemberApproval(value: unknown): MemberApprovalDetail {
  const at = "session.collaboration.approval.get"
  const source = obj(value, at)
  if (source.state === "ended") return { state: "ended" }
  if (source.state !== "pending") fail(at, "pending or ended")
  const descriptor = obj(source.descriptor, `${at}.descriptor`)
  const member = obj(descriptor.member, `${at}.member`)
  const call = obj(descriptor.call, `${at}.call`)
  return { state: "pending", approvalId: str(source, "approvalId", at), descriptor: {
    delegationId: str(descriptor, "delegationId", at), requestId: str(descriptor, "requestId", at), reason: str(descriptor, "reason", at),
    member: { agentId: str(member, "agentId", at), sessionId: str(member, "sessionId", at), branchId: str(member, "branchId", at) },
    call: { name: str(call, "name", at), input: obj(call.input, at) },
  } }
}

/** Decodes a server notification; methods the web workspace does not use read as undefined. */
export function readNotification(method: string, value: unknown): Notification | undefined {
  const params = obj(value ?? {}, method)
  switch (method) {
    case "session.event":
      return {
        method,
        sessionId: str(params, "sessionId", method),
        branchId: str(params, "branchId", method),
        event: readEvent(params.event, `${method}.event`),
      }
    case "session.state":
      return {
        method,
        sessionId: str(params, "sessionId", method),
        branchId: str(params, "branchId", method),
        running: bool(params, "running", method),
        stopping: bool(params, "stopping", method),
      }
    case "session.model.progress": {
      const requestId = optStr(params, "requestId", method)
      return {
        method,
        progress: {
          sessionId: str(params, "sessionId", method),
          branchId: str(params, "branchId", method),
          ...(requestId === undefined ? {} : { requestId }),
          text: str(params, "text", method),
          textOffset: num(params, "textOffset", method),
          thinking: str(params, "thinking", method),
          thinkingOffset: num(params, "thinkingOffset", method),
        },
      }
    }
    case "approval.request":
      return { method, request: readApproval(params) }
    case "approval.resolved":
    case "approval.cancelled":
      return {
        method,
        sessionId: str(params, "sessionId", method),
        branchId: str(params, "branchId", method),
        requestId: str(params, "requestId", method),
      }
    case "session.notice": {
      if (params.source !== "Collaboration" || params.kind !== "memberApprovalEnded") return undefined
      const data = obj(params.data, method)
      return { method: "memberApprovalEnded", sessionId: str(params, "sessionId", method), branchId: str(data, "branchId", method), approvalId: str(data, "approvalId", method) }
    }
    case "session.closed": {
      const reason = optStr(params, "reason", method)
      return { method, sessionId: str(params, "sessionId", method), ...(reason === undefined ? {} : { reason }) }
    }
    default:
      return undefined
  }
}
