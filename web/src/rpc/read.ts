/**
 * Strict readers for App Server replies and notifications: the one place the
 * web workspace parses wire data. A value that does not match its contract
 * throws a ProtocolError here; nothing past this file re-checks shapes.
 */
import type {
  ApprovalRequest,
  CreateResult,
  EventContent,
  HelloResult,
  HistoryPage,
  ModelId,
  ModelUpdateResult,
  Notification,
  ProviderCatalog,
  ProviderSummary,
  SessionEvent,
  SessionSummary,
  ToolResult,
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

function bool(source: Json, key: string, at: string): boolean {
  const value = source[key]
  if (typeof value !== "boolean") fail(`${at}.${key}`, "a boolean")
  return value
}

function modelId(value: unknown, at: string): ModelId {
  const source = obj(value, at)
  return { providerId: str(source, "providerId", at), modelId: str(source, "modelId", at) }
}

// ---------------------------------------------------------------- replies

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

/** `provider.catalog`: `{catalog: null}` when the provider has no catalog. */
export function readProviderCatalog(value: unknown): ProviderCatalog | undefined {
  const source = obj(value, "provider.catalog")
  if (source.catalog === null) return undefined
  const catalog = obj(source.catalog, "provider.catalog.catalog")
  return {
    models: arr(catalog.models, "provider.catalog.models").map((item, index) => {
      const at = `provider.catalog.models[${index}]`
      const model = obj(item, at)
      const unavailableReason = optStr(model, "unavailableReason", at)
      return { id: str(model, "id", at), name: str(model, "name", at), ...(unavailableReason === undefined ? {} : { unavailableReason }) }
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

// ----------------------------------------------------------------- events

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

function content(type: string, name: string, data: Json, at: string): EventContent {
  switch (`${type}/${name}`) {
    case "input/message": {
      // An input without `user` only carries context blocks, not a message.
      if (data.user === undefined || data.user === null) return { kind: "input" }
      return { kind: "input", text: textOf(obj(data.user, `${at}.user`).content, `${at}.user.content`) }
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

function readEvent(value: unknown, at: string): SessionEvent {
  const source = obj(value, at)
  const type = str(source, "type", at)
  const name = str(source, "name", at)
  return {
    sequence: num(source, "sequence", at),
    createAt: num(source, "create_at", at),
    content: content(type, name, obj(source.data, `${at}.data`), `${at}.data`),
  }
}

// ---------------------------------------------------------- notifications

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
    case "approval.cancelled":
      return {
        method,
        sessionId: str(params, "sessionId", method),
        branchId: str(params, "branchId", method),
        requestId: str(params, "requestId", method),
      }
    case "session.closed": {
      const reason = optStr(params, "reason", method)
      return { method, sessionId: str(params, "sessionId", method), ...(reason === undefined ? {} : { reason }) }
    }
    default:
      return undefined
  }
}
