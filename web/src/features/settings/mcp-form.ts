import type { McpConfigurationPatch, McpEditableEntry, McpOAuthInput, McpServerInput } from "../../rpc/general-settings"

export type SecretField = { name: string; action: "keep" | "set" | "remove"; value: string; stored?: boolean }
export type Draft = {
  id: string; source: "user" | "project"; type: "stdio" | "http" | "sse"
  command: string; args: string[]; cwd: string; url: string; endpoint?: string
  argsAction: "keep" | "replace"; storedArgsCount: number
  secrets: SecretField[]; enabled: boolean; token: string; authConfigured: boolean
  bearerAction: "keep" | "set" | "remove"; oauth: boolean; oauthAction: "keep" | "set" | "remove"
  clientId: string; callbackPort: string; scopes: string; metadataUrl: string
  timeout: string | number
  metadataConfigured: boolean; metadataAction: "keep" | "set" | "remove"
  secretsInUserScope: boolean
}

// A number-typed input reaches the draft as a number; a blank field states no limit.
function timeoutValue(value: string | number | undefined): number | null {
  const text = String(value ?? "").trim()
  return text ? Number(text) : null
}

export function configuration(form: Draft): McpServerInput {
  const values = Object.fromEntries(form.secrets.filter((secret) => secret.action === "set").map((secret) => [secret.name.trim(), secret.value]))
  const enabled = form.source === "user" && form.enabled
  const timeout = timeoutValue(form.timeout)
  if (form.type === "stdio") return { type: "stdio", command: form.command, args: form.args, ...(form.cwd ? { cwd: form.cwd } : {}), env: values, enabled, ...(timeout === null ? {} : { timeout }) }
  const oauth: McpOAuthInput = {
    ...(form.clientId.trim() ? { clientId: form.clientId.trim() } : {}),
    ...(form.callbackPort ? { callbackPort: Number(form.callbackPort) } : {}),
    ...(form.scopes.trim() ? { scopes: form.scopes.trim() } : {}),
    ...(form.metadataUrl.trim() ? { authServerMetadataUrl: form.metadataUrl.trim() } : {}),
  }
  return { type: form.type, url: form.url.trim(), headers: values, enabled, ...(form.token ? { auth: { type: "bearer", token: form.token } as const } : {}), ...(form.oauth ? { oauth } : {}), ...(timeout === null ? {} : { timeout }) }
}

export function patch(form: Draft): McpConfigurationPatch {
  // A blank field clears the entry's limit, so every save states it.
  const limit = { timeout: timeoutValue(form.timeout) }
  if (form.secretsInUserScope) {
    if (form.type === "stdio") return { command: form.command, cwd: form.cwd, ...limit }
    const value: McpConfigurationPatch = { ...limit }
    if (form.oauthAction === "remove") value.oauth = { action: "remove" }
    if (form.oauthAction === "set") value.oauth = {
      action: "set", clientId: form.clientId.trim() || null, callbackPort: form.callbackPort ? Number(form.callbackPort) : null, scopes: form.scopes.trim() || null,
    }
    return value
  }
  const secrets = Object.fromEntries(form.secrets.filter((secret) => secret.action !== "keep").map((secret) => [secret.name.trim(), secret.action === "remove" ? null : secret.value]))
  if (form.type === "stdio") return { command: form.command, ...(form.argsAction === "replace" ? { args: form.args } : {}), cwd: form.cwd, env: secrets, ...limit }
  const value: McpConfigurationPatch = { ...(form.url.trim() ? { url: form.url.trim() } : {}), headers: secrets, ...limit }
  if (form.bearerAction !== "keep") value.bearer = form.bearerAction === "remove" ? { action: "remove" } : { action: "set", token: form.token }
  if (form.oauthAction === "remove") value.oauth = { action: "remove" }
  if (form.oauthAction === "set") value.oauth = {
    action: "set", clientId: form.clientId.trim() || null, callbackPort: form.callbackPort ? Number(form.callbackPort) : null, scopes: form.scopes.trim() || null,
    ...(form.metadataAction === "remove" ? { authServerMetadataUrl: null } : form.metadataAction === "set" ? { authServerMetadataUrl: form.metadataUrl.trim() } : {}),
  }
  return value
}

export function blankMcpDraft(): Draft {
  return {
    id: "", source: "user", type: "stdio", command: "", args: [], argsAction: "replace", storedArgsCount: 0,
    cwd: "", url: "", secrets: [], enabled: false, oauth: false, clientId: "", token: "", bearerAction: "keep",
    authConfigured: false, oauthAction: "keep", callbackPort: "", scopes: "", metadataUrl: "",
    metadataConfigured: false, metadataAction: "keep", secretsInUserScope: false, timeout: "",
  }
}

export function draftFromEntry(id: string, source: Draft["source"], entry: McpEditableEntry): Draft {
  const config = entry.configuration
  return {
    id, source, type: config.type,
    command: config.type === "stdio" ? config.command : "", args: [], argsAction: "keep", storedArgsCount: config.type === "stdio" ? config.argsCount : 0, cwd: config.type === "stdio" ? config.cwd ?? "" : "",
    url: "", ...(config.type !== "stdio" ? { endpoint: config.endpoint } : {}),
    secrets: (config.type === "stdio" ? config.envNames : config.headerNames).map((name) => ({ name, action: "keep", value: "", stored: true })),
    enabled: entry.enabled, oauth: config.type !== "stdio" && config.oauthConfigured,
    authConfigured: config.type !== "stdio" && config.authConfigured,
    clientId: config.type !== "stdio" ? config.oauth?.clientId ?? "" : "",
    callbackPort: config.type !== "stdio" ? String(config.oauth?.callbackPort ?? "") : "",
    scopes: config.type !== "stdio" ? config.oauth?.scopes ?? "" : "",
    metadataConfigured: config.type !== "stdio" && (config.oauth?.metadataConfigured ?? false),
    metadataUrl: "", metadataAction: "keep", token: "", bearerAction: "keep", oauthAction: "keep",
    timeout: config.timeout === undefined ? "" : String(config.timeout),
    secretsInUserScope: entry.secretsInUserScope,
  }
}
