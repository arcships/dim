import { FieldSelect } from "./field-select"
import { Button } from "@heroui/react"
import { useLayoutEffect, useState, type FormEvent } from "react"
import { RpcError } from "../rpc/client"
import { readManagementProvider, readManagementUpdateSummary, readProbe, type ManagementInfo, type ManagementPage, type ModelEntry } from "../rpc/management"
import { connection } from "./stores"

type Header = { name: string; value: string; stored: boolean; remove: boolean }
type Draft = {
  id: string; name: string; driver: string; auth: string; headerName: string; prefix: string; baseUrl: string
  key: string; accessKey: string; sessionToken: string; clearKey: boolean; enabled: boolean
  proxy: string; timeout: string; modelPrefix: string; headers: Header[]
}
type Credential = { action: "clear" } | { action: "set"; value: string }
  | { action: "set_aws"; value: { accessKeyId: string; secretAccessKey: string; sessionToken?: string } }
type FieldOperation<T> = { action: "set"; value: T } | { action: "clear" }
type ConnectionPatch = {
  displayName?: string; baseUrl?: string; credential?: Credential; enabled?: boolean; gluedPrefix?: string | null
  headers?: ({ action: "set"; name: string; value: string } | { action: "remove"; name: string })[]
  network?: { proxy?: FieldOperation<string>; timeoutMs?: FieldOperation<number> }
}
type Text = (en: string, zh: string) => string

function fromProvider(provider: ManagementInfo | undefined, drivers: ManagementPage["drivers"]): Draft {
  const current = provider?.connection
  const driver = current?.driverKind ?? drivers.find(({ driverKind }) => driverKind === "openai-compatible")?.driverKind ?? drivers[0]?.driverKind ?? ""
  const authTypes = drivers.find(({ driverKind }) => driverKind === driver)?.authTypes ?? []
  return {
    id: provider?.providerId ?? crypto.randomUUID(),
    name: provider?.displayName ?? "",
    driver,
    auth: current?.auth.type ?? (authTypes.includes("bearer") ? "bearer" : authTypes[0] ?? ""),
    headerName: current?.auth.headerName ?? "Authorization", prefix: current?.auth.prefix ?? "Bearer ",
    baseUrl: current?.baseUrl ?? "", key: "", accessKey: "", sessionToken: "", clearKey: false,
    enabled: provider?.enabled ?? true, proxy: current?.proxyUrl ?? "", timeout: current?.timeoutMs?.toString() ?? "",
    modelPrefix: current?.gluedPrefix ?? "",
    headers: (current?.headerNames ?? []).map((name) => ({ name, value: "", stored: true, remove: false })),
  }
}

function credential(draft: Draft): Credential | undefined {
  if (draft.clearKey) return { action: "clear" }
  if (draft.auth === "aws_bedrock" && (draft.accessKey || draft.key || draft.sessionToken)) return {
    action: "set_aws", value: { accessKeyId: draft.accessKey, secretAccessKey: draft.key,
      ...(draft.sessionToken ? { sessionToken: draft.sessionToken } : {}) },
  }
  if (draft.key && draft.auth !== "anonymous") return { action: "set", value: draft.key }
}

function connectionPatch(draft: Draft, initial: Draft, provider: ManagementInfo): ConnectionPatch {
  const changes: ConnectionPatch = {}
  if (provider.source === "custom" && draft.name !== initial.name) changes.displayName = draft.name
  if (provider.credentialAuthority === "static") {
    if (draft.baseUrl !== initial.baseUrl) changes.baseUrl = draft.baseUrl
    const key = credential(draft)
    if (key) changes.credential = key
    const headers = draft.headers.flatMap<NonNullable<ConnectionPatch["headers"]>[number]>(({ name, value, remove, stored }) => {
      if (remove) return stored ? [{ action: "remove", name }] : []
      return value ? [{ action: "set", name, value }] : []
    })
    if (headers.length) changes.headers = headers
  }
  if (draft.modelPrefix !== initial.modelPrefix) changes.gluedPrefix = draft.modelPrefix.trim() || null
  if (draft.enabled !== initial.enabled) changes.enabled = draft.enabled
  if (draft.proxy !== initial.proxy || draft.timeout !== initial.timeout) changes.network = {
    ...(draft.proxy !== initial.proxy ? { proxy: draft.proxy ? { action: "set", value: draft.proxy } : { action: "clear" } } : {}),
    ...(draft.timeout !== initial.timeout ? { timeoutMs: draft.timeout ? { action: "set", value: Number(draft.timeout) } : { action: "clear" } } : {}),
  }
  return changes
}

function createConnection(draft: Draft) {
  const key = credential(draft)
  return {
    driverKind: draft.driver, baseUrl: draft.baseUrl,
    auth: { type: draft.auth, ...(["api_key", "custom_header"].includes(draft.auth) ? { headerName: draft.headerName } : {}),
      ...(draft.auth === "api_key" ? { prefix: draft.prefix } : {}) },
    ...(key ? { credential: key } : {}),
    headers: Object.fromEntries(draft.headers.filter(({ name, value, remove }) => name && value && !remove).map(({ name, value }) => [name, value])),
    ...(draft.proxy || draft.timeout ? { network: { ...(draft.proxy ? { proxy: draft.proxy } : {}), ...(draft.timeout ? { timeoutMs: Number(draft.timeout) } : {}) } } : {}),
  }
}

export function ProviderConnection({ provider, drivers, online, text, onDirty, onSaved, onReload, onCancel }: {
  provider?: ManagementInfo; drivers: ManagementPage["drivers"]; online: boolean; text: Text
  onDirty: (dirty: boolean) => void; onSaved: (provider: ManagementInfo) => void
  onReload: () => void; onCancel: () => void
}) {
  const [initial] = useState(() => fromProvider(provider, drivers))
  const [draft, setDraft] = useState(initial)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [conflict, setConflict] = useState(false)
  const [probed, setProbed] = useState<ModelEntry[]>()
  const dirty = JSON.stringify(draft) !== JSON.stringify(initial)
  const managed = provider !== undefined && provider.credentialAuthority !== "static"
  const driver = drivers.find(({ driverKind }) => driverKind === draft.driver)
  const canSave = online && !busy && !conflict && connection.supports(provider ? "provider.update" : "provider.create")
    && (provider ? provider.actions.includes("update") && dirty : !!draft.auth && !!draft.driver)
  const usesStoredKeyAtNewUrl = provider?.connection?.credentialConfigured && draft.baseUrl !== initial.baseUrl && !credential(draft)
  useLayoutEffect(() => {
    onDirty(dirty)
    return () => onDirty(false)
  }, [dirty, onDirty])

  function edit(changes: Partial<Draft>) {
    setDraft((current) => ({ ...current, ...changes }))
    setProbed(undefined)
  }
  function editHeader(index: number, changes: Partial<Header>) {
    edit({ headers: draft.headers.map((header, position) => position === index ? { ...header, ...changes } : header) })
  }

  function confirmEndpoint() {
    return !usesStoredKeyAtNewUrl || window.confirm(text("Use the stored credential at the new API URL?", "在新的 API URL 使用已存储的凭据？"))
  }

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!canSave || !confirmEndpoint()) return
    setBusy(true)
    setError("")
    try {
      const result = provider
        ? await connection.client.call("provider.update", {
          providerId: provider.providerId, expectedVersion: provider.version, patch: connectionPatch(draft, initial, provider), reply: "summary",
        }, readManagementUpdateSummary)
        : await connection.client.call("provider.create", {
          providerId: draft.id, displayName: draft.name, enabled: draft.enabled, connection: createConnection(draft), models: probed ?? [],
          ...(draft.modelPrefix.trim() ? { gluedPrefix: draft.modelPrefix.trim() } : {}),
        }, readManagementProvider)
      onDirty(false)
      onSaved(result)
    } catch (cause) {
      const stale = cause instanceof RpcError && cause.appError === "provider_conflict"
      setConflict(stale)
      setError(stale ? text("Provider changed. Reload before saving again. Your draft is still here.", "供应商已更改。请重新加载后再保存，当前草稿已保留。")
        : cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  async function probe() {
    if (!online || busy || !draft.baseUrl || !connection.supports("provider.probe") || !confirmEndpoint()) return
    setBusy(true)
    setError("")
    setProbed(undefined)
    try {
      setProbed(await connection.client.call("provider.probe", {
        providerId: draft.id, connection: provider ? connectionPatch(draft, initial, provider) : createConnection(draft),
      }, readProbe))
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  return <form className="rw-settings-card rw-settings-fields" autoComplete="off" onSubmit={save}>
    <h2>{provider ? text("Connection", "连接") : text("Custom provider", "自定义供应商")}</h2>
    <fieldset className="rw-mcp-fields" disabled={busy || !online || (provider !== undefined && !provider.actions.includes("update"))}>
      {(!provider || provider.source === "custom") && <label>{text("Provider name", "供应商名称")}
        <input required value={draft.name} onChange={(event) => edit({ name: event.target.value })} />
      </label>}
      {!managed && <label>{text("API URL", "API URL")}
        <input type="url" required value={draft.baseUrl} onChange={(event) => edit({ baseUrl: event.target.value })} />
      </label>}
      {!provider && <>
        <div className="rw-select-field">{text("API protocol", "API 协议")}
          <FieldSelect label={text("API protocol", "API 协议")} value={draft.driver}
            onChange={(selectedValue) => {
              const driver = drivers.find(({ driverKind }) => driverKind === selectedValue)
              if (!driver) return
              edit({ driver: driver.driverKind, auth: driver.authTypes.includes(draft.auth) ? draft.auth : driver.authTypes[0] ?? "",
                key: "", accessKey: "", sessionToken: "" })
            }} options={drivers.map(({ driverKind }) => ({ value: driverKind, label: driverKind }))} />
        </div>
        <div className="rw-select-field">{text("Authentication", "身份验证")}
          <FieldSelect label={text("Authentication", "身份验证")} value={draft.auth}
            onChange={(auth) => edit({ auth, key: "", accessKey: "", sessionToken: "" })}
            options={driver?.authTypes.map((kind) => ({ value: kind, label: kind })) ?? []} />
        </div>
        {["api_key", "custom_header"].includes(draft.auth) && <label>{text("Authentication header name", "身份验证请求头名称")}
          <input required value={draft.headerName} onChange={(event) => edit({ headerName: event.target.value })} />
        </label>}
        {draft.auth === "api_key" && <label>{text("Header value prefix", "请求头值前缀")}
          <input value={draft.prefix} onChange={(event) => edit({ prefix: event.target.value })} />
        </label>}
      </>}
      {!managed && <>
        {draft.auth === "aws_bedrock" && <label>AWS Access Key ID
          <input type="password" autoComplete="new-password" disabled={draft.clearKey} value={draft.accessKey} onChange={(event) => edit({ accessKey: event.target.value })} />
        </label>}
        {draft.auth !== "anonymous" && <label>{draft.auth === "aws_bedrock" ? "AWS Secret Access Key" : text("API credential", "API 凭据")}
          <input type="password" autoComplete="new-password" disabled={draft.clearKey} value={draft.key} onChange={(event) => edit({ key: event.target.value })}
            placeholder={provider?.connection?.credentialConfigured ? text("Blank keeps the stored credential", "留空保留已存凭据") : undefined} />
        </label>}
        {draft.auth === "aws_bedrock" && <label>AWS Session Token
          <input type="password" autoComplete="new-password" disabled={draft.clearKey} value={draft.sessionToken} onChange={(event) => edit({ sessionToken: event.target.value })} />
        </label>}
        {provider?.connection?.credentialConfigured && <label className="rw-recording-toggle">
          <input type="checkbox" checked={draft.clearKey} onChange={(event) => edit({ clearKey: event.target.checked, key: "", accessKey: "", sessionToken: "" })} />
          {text("Clear stored credential", "清除已存凭据")}
        </label>}
        <details className="rw-provider-options">
          <summary>{text("Headers", "请求头")}</summary>
          {draft.headers.map((header, index) => <div className="rw-field-row rw-credential-row" key={index}>
            <label>{text("Header name", "请求头名称")}
              <input value={header.name} readOnly={header.stored} disabled={header.remove} onChange={(event) => editHeader(index, { name: event.target.value })} />
            </label>
            <label>{text("Header value (write-only)", "请求头值（只写）")}
              <input type="password" autoComplete="new-password" value={header.value} disabled={header.remove}
                onChange={(event) => editHeader(index, { value: event.target.value })} />
            </label>
            <Button size="sm" type="button" variant="tertiary" onPress={() => {
              if (header.stored) editHeader(index, { remove: !header.remove, value: "" })
              else edit({ headers: draft.headers.filter((_, position) => position !== index) })
            }}>{header.remove ? text("Keep header", "保留请求头") : text("Remove header", "移除请求头")}</Button>
          </div>)}
          <Button size="sm" type="button" variant="secondary" onPress={() => edit({
            headers: [...draft.headers, { name: "", value: "", stored: false, remove: false }],
          })}>{text("Add header", "添加请求头")}</Button>
        </details>
      </>}
      <details className="rw-provider-options">
        <summary>{text("Advanced", "高级")}</summary>
        <label>{text("Proxy URL", "代理 URL")}<input type="url" value={draft.proxy} onChange={(event) => edit({ proxy: event.target.value })} /></label>
        <label>{text("Timeout (milliseconds)", "超时（毫秒）")}
          <input type="number" min="1" step="1" value={draft.timeout} onChange={(event) => edit({ timeout: event.target.value })} />
        </label>
        <label>{text("Model ID prefix", "模型 ID 前缀")}
          <input value={draft.modelPrefix} onChange={(event) => edit({ modelPrefix: event.target.value })} />
        </label>
      </details>
      <label className="rw-recording-toggle">
        <input type="checkbox" checked={draft.enabled} onChange={(event) => edit({ enabled: event.target.checked })} />{text("Enable provider", "启用供应商")}
      </label>
    </fieldset>
    {error && <p role="alert" className="rw-error">{error}</p>}
    {conflict && <Button size="sm" type="button" variant="secondary" isDisabled={!online || busy} onPress={onReload}>{text("Reload provider", "重新加载供应商")}</Button>}
    {probed && <p role="status">{text(`Connection test found ${probed.length} models.`, `连接测试找到 ${probed.length} 个模型。`)}</p>}
    <div className="rw-setting-options">
      <Button size="sm" type="button" variant="secondary" isDisabled={busy} onPress={onCancel}>{text("Cancel", "取消")}</Button>
      {!managed && connection.supports("provider.probe") && <Button size="sm" type="button" variant="secondary"
        isDisabled={!online || busy || !draft.baseUrl} onPress={() => void probe()}>{text("Test connection", "测试连接")}</Button>}
      <Button size="sm" type="submit" isPending={busy} isDisabled={!canSave}>{text("Save provider", "保存供应商")}</Button>
    </div>
  </form>
}
