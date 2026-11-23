import { FieldSelect } from "./field-select"
import { Button } from "@heroui/react"
import { useEffect, useLayoutEffect, useRef, useState, type FormEvent } from "react"
import { blankMcpDraft, configuration, draftFromEntry, patch, type Draft } from "../features/settings/mcp-form"
import { RpcError } from "../rpc/client"
import {
  readMcp, readMcpAuthStart, readMcpEditableEntry, readMcpInspection, readMcpTest,
  type McpEntry, type McpInspection, type McpInvalidEntry, type McpProbe, type McpView,
} from "../rpc/general-settings"
import { readAck } from "../rpc/read"
import { McpEditor } from "./mcp-editor"
import { McpImport } from "./mcp-import"
import { connection, sessions, useStoreValue } from "./stores"

const readOnline = () => connection.phase === "connected"
const readWorkspaces = () => sessions.workspaces
type Entry = Pick<McpEntry, "id" | "source">
type Editable = { id: string; source: Draft["source"] }
type Editor = { draft: Draft; baseline: Draft; revision?: string }
type AuthAttempt = ReturnType<typeof readMcpAuthStart> & { server: string }
const isEditable = (entry: Entry): entry is Editable => entry.source !== "plugin"
const sameEntry = (a: Entry | undefined, b: Entry) => a?.id === b.id && a.source === b.source

export function McpSettings({ text, detail, onDirty }: {
  text: (en: string, zh: string) => string
  detail?: string
  onDirty: (dirty: boolean) => void
}) {
  const online = useStoreValue(readOnline)
  const workspaces = useStoreValue(readWorkspaces)
  const [loaded, setLoaded] = useState<{ cwd: string; catalog: McpView }>()
  const [editor, setEditor] = useState<Editor>()
  const [inspection, setInspection] = useState<{ server: McpEntry; result: McpInspection }>()
  const [probe, setProbe] = useState<{ id: string; result: McpProbe }>()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [status, setStatus] = useState("")
  const [conflict, setConflict] = useState(false)
  const [withheld, setWithheld] = useState(false)
  const [query, setQuery] = useState(detail ?? "")
  const [authorization, setAuthorization] = useState<AuthAttempt>()
  const [clientSecret, setClientSecret] = useState("")
  const [callback, setCallback] = useState("")
  const authRef = useRef<AuthAttempt | undefined>(undefined)
  const mounted = useRef(false)
  const cwd = loaded?.cwd ?? ""
  const catalog = loaded?.catalog
  const scope = cwd ? { cwd } : {}
  const dirty = editor !== undefined && JSON.stringify(editor.draft) !== JSON.stringify(editor.baseline)
  const authDirty = Boolean(authorization || clientSecret || callback)
  const permitted = (method: string) => online && !busy && connection.supports(method)
  const editing = editor?.revision !== undefined
  const queryKey = query.trim().toLowerCase()
  const servers = catalog?.servers.filter(({ id }) => id.toLowerCase().includes(queryKey)) ?? []
  const invalid = catalog?.invalid.filter(({ id }) => id.toLowerCase().includes(queryKey)) ?? []
  const target = ({ id, source }: Entry) => ({ ...scope, id, source })

  useLayoutEffect(() => {
    onDirty(dirty || authDirty)
    return () => onDirty(false)
  }, [dirty, authDirty, onDirty])

  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
      const attempt = authRef.current
      if (attempt) void connection.client.call("mcp.auth.cancel", { attemptId: attempt.attemptId }, readAck).catch(() => {})
    }
  }, [])

  useEffect(() => setQuery(detail ?? ""), [detail])

  async function operate(operation: () => Promise<void>) {
    if (busy || !online) return
    setBusy(true)
    setError("")
    setStatus("")
    try {
      await operation()
    } catch (cause) {
      const code = cause instanceof RpcError ? cause.appError : undefined
      const stale = code === "mcp_move_conflict" || code === "mcp_settings_conflict"
      if (code === "mcp_credentials_are_user_scoped") setWithheld(true)
      if (stale) setConflict(true)
      if (stale || code === "mcp_move_partial") await loadCatalog().catch(() => {})
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  async function loadCatalog(path = cwd) {
    if (!connection.supports("mcp.catalog")) throw new Error(text("MCP management is unavailable on this server.", "当前服务器不支持 MCP 管理。"))
    const result = await connection.client.call("mcp.catalog", path ? { cwd: path } : {}, readMcp)
    setLoaded({ cwd: path, catalog: result })
    return result
  }

  function revision(source: Draft["source"]) {
    const value = source === "user" ? catalog?.userRevision : catalog?.projectRevision
    if (value === undefined) throw new Error(text("Load the source before editing its MCP settings.", "编辑前请先加载来源的 MCP 设置。"))
    return value
  }

  async function load(path = cwd) {
    if ((dirty || authDirty) && !window.confirm(text("Discard unsaved MCP configuration and authentication?", "放弃未保存的 MCP 配置和认证？"))) return
    await operate(async () => {
      if (authorization) await connection.client.call("mcp.auth.cancel", { attemptId: authorization.attemptId }, readAck)
      authRef.current = undefined
      setAuthorization(undefined)
      setCallback("")
      setClientSecret("")
      await loadCatalog(path)
      setEditor(undefined)
      setInspection(undefined)
      setProbe(undefined)
      setConflict(false)
      setWithheld(false)
    })
  }

  useEffect(() => {
    if (online && !editor && !authDirty) void load()
  }, [online])

  function create() {
    const draft = blankMcpDraft()
    setEditor({ draft, baseline: draft })
    setProbe(undefined)
    setConflict(false)
    setWithheld(false)
  }

  async function readEditor(server: Editable) {
    const entry = await connection.client.call("mcp.configuration", target(server), readMcpEditableEntry)
    if (entry.revision === undefined) throw new Error(text("This server has no editable source revision.", "此服务器没有可编辑的来源修订号。"))
    const draft = draftFromEntry(server.id, server.source, entry)
    setEditor({ draft, baseline: draft, revision: entry.revision })
    setWithheld(false)
    setConflict(false)
    setProbe(undefined)
  }

  function validDraft(form: HTMLFormElement, draft: Draft) {
    if (!form.reportValidity()) return false
    const names = draft.secrets.map(({ name }) => name.trim())
    if (names.some((name) => name === "") || new Set(names).size !== names.length) {
      setError(text("Field names must be nonempty and unique.", "字段名称不能为空且必须唯一。"))
      return false
    }
    const url = draft.url.trim()
    if (draft.type !== "stdio" && !draft.secretsInUserScope && url && !url.includes("${")) {
      try { new URL(url) }
      catch {
        setError(text("Enter an absolute URL or a variable reference the daemon can expand.", "请输入绝对 URL 或守护进程可展开的变量引用。"))
        return false
      }
    }
    return true
  }

  const editorForm = useRef<HTMLDivElement>(null)
  function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!editor || !dirty || conflict || !validDraft(event.currentTarget, editor.draft)) return
    const { draft, revision: expectedVersion } = editor
    const id = draft.id.trim()
    if (!editing && catalog?.servers.some((server) => server.id === id)) {
      setError(text("A server already uses this ID.", "已有服务器使用此 ID。"))
      return
    }
    if (!editing && draft.source === "user" && draft.enabled && !window.confirm(text(
      "Enable this server after saving? Its configured command or network connection can run.",
      "保存后启用此服务器？这会允许运行其配置的命令或网络连接。",
    ))) return
    void operate(async () => {
      if (expectedVersion !== undefined) await connection.client.call("mcp.update", {
        ...target(draft), expectedVersion, patch: patch(draft),
      }, readAck)
      else await connection.client.call("mcp.save", {
        ...scope, id, source: draft.source, expectedVersion: revision(draft.source), configuration: configuration(draft),
      }, readAck)
      setEditor(undefined)
      setStatus(text("Server saved.", "服务器已保存。"))
      await loadCatalog()
    })
  }

  function test(server?: McpEntry) {
    let params
    if (server) params = target(server)
    else {
      const form = editorForm.current?.querySelector("form")
      if (!editor || !form || !validDraft(form, editor.draft)) return
      params = { ...target(editor.draft), id: editor.draft.id.trim(),
        ...(editing ? { patch: patch(editor.draft) } : { configuration: configuration(editor.draft) }) }
    }
    if (!window.confirm(text(
      "Open a temporary connection to list tools? This executes the configured command or contacts its endpoint.",
      "打开临时连接以列出工具？这会执行配置的命令或连接端点。",
    ))) return
    void operate(async () => {
      setProbe(undefined)
      const result = await connection.client.call("mcp.test", params, readMcpTest)
      setProbe({ id: params.id, result })
    })
  }

  function inspect(server: McpEntry) {
    if (clientSecret && !window.confirm(text("Discard the entered OAuth secret?", "放弃输入的 OAuth 密钥？"))) return
    void operate(async () => {
      setInspection({ server, result: await connection.client.call("mcp.inspect", target(server), readMcpInspection) })
      setClientSecret("")
    })
  }

  function toggle(server: McpEntry & Editable) {
    if (!server.enabled && !window.confirm(text("Enable this server to run its configured command or network connection?", "启用此服务器运行其配置的命令或网络连接？"))) return
    void operate(async () => {
      await connection.client.call("mcp.set_enabled", { ...target(server), expectedVersion: revision(server.source), enabled: !server.enabled }, readAck)
      await loadCatalog()
      if (sameEntry(inspection?.server, server)) setInspection(undefined)
    })
  }

  function remove(server: Editable) {
    if (!window.confirm(text("Remove this server configuration?", "移除此服务器配置？"))) return
    void operate(async () => {
      await connection.client.call("mcp.remove", { ...target(server), expectedVersion: revision(server.source) }, readAck)
      await loadCatalog()
      if (sameEntry(inspection?.server, server)) setInspection(undefined)
    })
  }

  function move(server: Editable, editDestination = false) {
    const from = server.source
    const to = from === "user" ? "project" : "user"
    if (!cwd || (dirty && !window.confirm(text("Discard unsaved MCP configuration?", "放弃未保存的 MCP 配置？")))) return
    if (!window.confirm(to === "project"
      ? text(`Move “${server.id}” to ${cwd}? The workspace receives public configuration. Withheld values stay in your user configuration and the moved server starts disabled.`,
        `将「${server.id}」移至 ${cwd}？工作区只接收公开配置。隐藏值留在用户配置中，迁移后服务器默认禁用。`)
      : text(`Move “${server.id}” from ${cwd} to user configuration? The server keeps its enablement.`,
        `将「${server.id}」从 ${cwd} 移到用户配置？服务器保留启用状态。`))) return
    void operate(async () => {
      await connection.client.call("mcp.move", { ...scope, id: server.id, from, to, expectedFromVersion: revision(from), expectedToVersion: revision(to) }, readAck)
      setEditor(undefined)
      setInspection(undefined)
      const result = await loadCatalog()
      const destination = result.servers.find(({ id, source }) => id === server.id && source === to)
      if (destination && editDestination) await readEditor({ id: destination.id, source: to })
      else if (destination && connection.supports("mcp.inspect")) setInspection({
        server: destination, result: await connection.client.call("mcp.inspect", target(destination), readMcpInspection),
      })
    })
  }

  function act(method: "mcp.disconnect" | "mcp.auth.logout", server: McpEntry) {
    void operate(async () => {
      await connection.client.call(method, target(server), readAck)
      await loadCatalog()
      if (sameEntry(inspection?.server, server)) setInspection(undefined)
    })
  }

  function startAuth(server: McpEntry) {
    void operate(async () => {
      const secret = sameEntry(inspection?.server, server) ? clientSecret : ""
      setClientSecret("")
      const result = await connection.client.call("mcp.auth.start", { ...target(server), ...(secret ? { clientSecret: secret } : {}) }, readMcpAuthStart)
      if (!mounted.current) {
        await connection.client.call("mcp.auth.cancel", { attemptId: result.attemptId }, readAck)
        return
      }
      const attempt = { ...result, server: server.id }
      authRef.current = attempt
      setAuthorization(attempt)
    })
  }

  function finishAuth(cancel: boolean) {
    if (!authorization || (!cancel && !callback.trim())) return
    void operate(async () => {
      await connection.client.call(cancel ? "mcp.auth.cancel" : "mcp.auth.complete", {
        attemptId: authorization.attemptId, ...(!cancel ? { callbackUrl: callback } : {}),
      }, readAck)
      authRef.current = undefined
      setAuthorization(undefined)
      setCallback("")
      await loadCatalog()
    })
  }

  return <article className="rw-settings">
    <h1>MCP</h1>
    <section className="rw-settings-card">
      <div className="rw-select-field">
        {text("Configuration scope", "配置范围")}
        <FieldSelect label={text("Configuration scope", "配置范围")} value={cwd} disabled={busy || !online}
          onChange={(selectedValue) => void load(selectedValue)} options={[
            { value: "", label: text("User configuration", "用户配置") },
            ...(cwd && !workspaces.some((workspace) => workspace.cwd === cwd) ? [{ value: cwd, label: cwd }] : []),
            ...workspaces.map(({ name, cwd }) => ({ value: cwd, label: `${name} — ${cwd}` })),
          ]} />
      </div>
      <div className="rw-setting-options">
        <Button size="sm" variant="secondary" isPending={busy} isDisabled={!online} onPress={() => void load()}>{text("Refresh MCP servers", "刷新 MCP 服务器")}</Button>
        <Button size="sm" isDisabled={!catalog || !!editor || !permitted("mcp.save")} onPress={create}>{text("Add server", "添加服务器")}</Button>
      </div>
    </section>
    <label className="rw-setting-language">{text("Search servers", "搜索服务器")}
      <input type="search" value={query} onChange={(event) => setQuery(event.target.value)} />
    </label>
    {error && <p className="rw-error" role="alert">{error}</p>}
    {conflict && <p>{text("The source changed. Refresh before saving again. Your draft is still here.", "来源已更改，请刷新后再保存。当前草稿已保留。")}</p>}
    {status && <p role="status">{status}</p>}
    {catalog && <McpImport key={cwd} cwd={cwd} text={text} disabled={!!editor || authDirty || busy}
      permitted={permitted} operate={operate} refresh={loadCatalog} />}
    {catalog?.errors.map((error, index) => <p className="rw-error" key={index}>{error}</p>)}
    {editor && <div ref={editorForm}><McpEditor draft={editor.draft} dirty={dirty} editing={editing} workspace={cwd}
      enabled={online && !busy} canTest={permitted("mcp.test")} canSave={!conflict && permitted(editing ? "mcp.update" : "mcp.save")} text={text}
      update={(fields) => { setEditor({ ...editor, draft: { ...editor.draft, ...fields } }); setProbe(undefined) }}
      save={save} test={() => test()} cancel={() => {
        if (!dirty || window.confirm(text("Discard unsaved MCP configuration?", "放弃未保存的 MCP 配置？"))) {
          setEditor(undefined)
          setConflict(false)
          setWithheld(false)
        }
      }} move={(editor.draft.secretsInUserScope || withheld) && permitted("mcp.move") ? () => move(editor.draft, true) : undefined} />
      {withheld && !editor.draft.secretsInUserScope && <Button size="sm" variant="secondary" isDisabled={!permitted("mcp.move")}
        onPress={() => move(editor.draft, true)}>{text("Move to user to edit", "移到用户范围编辑")}</Button>}
    </div>}
    {inspection && <section className="rw-settings-card rw-settings-fields">
      <h2>{inspection.server.id} · {text("Details", "详情")}</h2>
      <p>{inspection.server.source} · {inspection.result.configuration.type}</p>
      {inspection.result.configuration.type === "stdio" ? <>
        <p><code>{inspection.result.configuration.command}</code> · {inspection.result.configuration.argsCount} {text("stored arguments", "个已存参数")}</p>
        <p>{text("Working directory", "工作目录")} · {inspection.result.configuration.cwd || text("Daemon directory", "守护进程目录")}</p>
        <p>{text("Environment names", "环境变量名")} · {inspection.result.configuration.envNames.join(", ") || "—"}</p>
      </> : <>
        <p><code>{inspection.result.configuration.endpoint}</code></p>
        <p>{text("Header names", "请求头名称")} · {inspection.result.configuration.headerNames.join(", ") || "—"}</p>
      </>}
      {inspection.result.configuration.timeout !== undefined && <p>
        {text("Tool timeout", "工具超时")} · {inspection.result.configuration.timeout} ms
      </p>}
      {inspection.result.connections.length === 0 && <p>{text("No live session connections.", "没有活动会话连接。")}</p>}
      {inspection.result.configuration.referencedVariables.length > 0 && <p>
        {text("Referenced variables", "引用变量")} · {inspection.result.configuration.referencedVariables.join(", ")}
      </p>}
      {inspection.result.configuration.unsetVariables.length > 0 && <p className="rw-error">
        {text("Unset variables", "未设置变量")} · {inspection.result.configuration.unsetVariables.join(", ")}
      </p>}
      {inspection.result.connections.map(({ state, tools }, index) => <div key={index}>
        <p>{state}</p>
        {tools?.map(({ name, description }) => <p key={name}><strong>{name}</strong>{description && <> · {description}</>}</p>)}
      </div>)}
      {inspection.result.configuration.type !== "stdio" && permitted("mcp.auth.start") && <label>
        {text("OAuth client secret (optional, write-only)", "OAuth 客户端密钥（可选，只写）")}
        <input type="password" autoComplete="new-password" value={clientSecret} onChange={(event) => setClientSecret(event.target.value)} />
      </label>}
      <Button size="sm" variant="secondary" isDisabled={busy} onPress={() => {
        if (clientSecret && !window.confirm(text("Discard the entered OAuth secret?", "放弃输入的 OAuth 密钥？"))) return
        setInspection(undefined)
        setClientSecret("")
      }}>{text("Close details", "关闭详情")}</Button>
    </section>}
    {probe && <section role="status" className="rw-settings-card">
      <h2>{probe.result.status === "connected" ? text("Connection test passed", "连接测试通过")
        : probe.result.status === "authRequired" ? text("Sign-in required", "需要登录") : text("Connection test failed", "连接测试失败")}</h2>
      <p>{probe.id} · {probe.result.latencyMs} ms</p>
      {probe.result.message && <p>{probe.result.message}</p>}
      {probe.result.missingScopes?.length ? <p>{text("Additional scopes", "额外权限范围")} · {probe.result.missingScopes.join(", ")}</p> : null}
      {probe.result.tools?.map(({ name, description }) => <p key={name}><strong>{name}</strong>{description && <> · {description}</>}</p>)}
    </section>}
    {authorization && <form className="rw-settings-card rw-settings-fields" autoComplete="off" onSubmit={(event) => { event.preventDefault(); finishAuth(false) }}>
      <h2>{text("Authorize MCP", "授权 MCP")} · {authorization.server}</h2>
      <p>{text("Callback", "回调")} · <code>{authorization.redirectUri}</code></p>
      <p>{text("Expires", "过期时间")} · {new Date(authorization.expiresAt).toLocaleString()}</p>
      <a className="rw-settings-link" href={authorization.authorizationUrl} target="_blank" rel="noopener noreferrer">{text("Open authorization page", "打开授权页面")}</a>
      <label>{text("Full callback URL", "完整回调 URL")}
        <input type="password" autoComplete="new-password" required value={callback} disabled={busy} onChange={(event) => setCallback(event.target.value)} />
      </label>
      <div className="rw-setting-options">
        <Button size="sm" type="submit" isDisabled={!callback.trim() || !permitted("mcp.auth.complete")}>{text("Complete sign-in", "完成登录")}</Button>
        <Button size="sm" type="button" variant="secondary" isDisabled={!permitted("mcp.auth.cancel")} onPress={() => finishAuth(true)}>{text("Cancel sign-in", "取消登录")}</Button>
      </div>
    </form>}
    {invalid.map((entry: McpInvalidEntry) => <section className="rw-settings-card" key={`${entry.source}:${entry.id}`}>
      <h2>{entry.id} · {entry.source}</h2>
      <p className="rw-error">{entry.error}</p>
      {isEditable(entry) && <Button size="sm" variant="danger-soft" isDisabled={!!editor || !permitted("mcp.remove")}
        onPress={() => remove(entry)}>{text("Remove invalid server", "移除无效服务器")}</Button>}
    </section>)}
    {servers.map((server) => <section className="rw-settings-card" key={`${server.source}:${server.id}`} aria-label={server.id}>
      <h2>{server.id}</h2>
      <p>{server.source} · {server.transport} · {server.enabled ? text("Enabled", "已启用") : text("Disabled", "已禁用")}
        {server.auth && <> · OAuth: {server.auth}</>}</p>
      {server.unsetVariables.length > 0 && <p className="rw-error">{text("Unset variables", "未设置变量")} · {server.unsetVariables.join(", ")}</p>}
      <div className="rw-setting-options">
        <Button size="sm" variant="secondary" isDisabled={!permitted("mcp.inspect")} onPress={() => inspect(server)}>{text("Details / tools", "详情 / 工具")}</Button>
        <Button size="sm" variant="secondary" isDisabled={!!editor || !permitted("mcp.test") || (server.source !== "user" && !server.enabled)}
          onPress={() => test(server)}>{text("Test connection", "测试连接")}</Button>
        {isEditable(server) && <>
          <Button size="sm" variant="secondary" isDisabled={!!editor || !permitted("mcp.configuration") || !connection.supports("mcp.update")}
            onPress={() => void operate(() => readEditor(server))}>{text("Edit", "编辑")}</Button>
          <Button size="sm" variant="secondary" isDisabled={!!editor || !cwd || !permitted("mcp.move")} onPress={() => move(server)}>
            {server.source === "user" ? text("Move to project…", "移入项目…") : text("Move to user…", "移到用户…")}
          </Button>
          <Button size="sm" variant="secondary" isDisabled={!!editor || !permitted("mcp.set_enabled")} onPress={() => toggle(server)}>
            {server.enabled ? text("Disable", "禁用") : text("Enable", "启用")}
          </Button>
          <Button size="sm" variant="danger-soft" isDisabled={!!editor || !permitted("mcp.remove")} onPress={() => remove(server)}>{text("Remove", "移除")}</Button>
        </>}
        <Button size="sm" variant="secondary" isDisabled={!server.enabled || !permitted("mcp.disconnect")}
          onPress={() => act("mcp.disconnect", server)}>{text("Disconnect", "断开连接")}</Button>
        {server.transport !== "stdio" && <>
          <Button size="sm" variant="secondary" isDisabled={!!authorization || !server.enabled || !permitted("mcp.auth.start")}
            onPress={() => startAuth(server)}>{text("Sign in", "登录")}</Button>
          {server.auth && <Button size="sm" variant="secondary" isDisabled={!permitted("mcp.auth.logout")}
            onPress={() => act("mcp.auth.logout", server)}>{text("Sign out", "退出登录")}</Button>}
        </>}
      </div>
      {server.source === "plugin" && <p>{text("Managed by its plugin.", "由所属插件管理。")}</p>}
    </section>)}
    {catalog && !servers.length && !invalid.length && <p role="status">{text("No matching MCP servers.", "没有匹配的 MCP 服务器。")}</p>}
  </article>
}
