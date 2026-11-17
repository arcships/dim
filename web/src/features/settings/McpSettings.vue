<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue"
import { useSettingsText } from "../../i18n/settings-general"
import { readMcp, readMcpAuthStart, readMcpEditableEntry, readMcpImportApplied, readMcpImportPreview, readMcpImportSources, readMcpInspection, readMcpTest, type McpConfigurationPatch, type McpEntry, type McpImportEntry, type McpImportPreviewSource, type McpImportResolution, type McpImportSource, type McpInspection, type McpInvalidEntry, type McpProbe, type McpServerInput, type McpView } from "../../rpc/general-settings"
import { RpcError } from "../../rpc/client"
import { configuration, patch, type Draft } from "./mcp-form"
import { readAck } from "../../rpc/read"
import { useConnectionStore } from "../../stores/connection"
import { useSessionsStore } from "../../stores/sessions"
import GeneralSettingsStatus from "./GeneralSettingsStatus.vue"
import { bindSettingsDirty, messageFrom, reloadSettingsOnReconnect } from "./context"

const s = useSettingsText()
const connection = useConnectionStore()
const sessions = useSessionsStore()
const catalog = ref<McpView>()
const loadedCwd = ref("")
const busy = ref(false)
const error = ref("")
const draft = ref<Draft>()
const baseline = ref("")
const editing = ref(false)
const editorRevision = ref("")
const authorization = ref<ReturnType<typeof readMcpAuthStart>>()
const authorizingId = ref("")
const callback = ref("")
const clientSecret = ref("")
const detail = ref<{ server: McpEntry; inspection: McpInspection }>()
const testResult = ref<{ id: string; probe: McpProbe }>()
type ImportEntryState = McpImportEntry & { action: "import" | "skip" | "replace" | "rename"; newId: string }
/// A preview names its input: the file's text the page read, or a known
/// store id the daemon reads. A discovered store's text is never here.
type ImportState = { source: "user" | "project"; document: string | undefined; from: string | undefined; store: McpImportPreviewSource | undefined; sourceRevision: string | undefined; revision: string; entries: ImportEntryState[]; unknownTopLevel: string[] }
const importPreview = ref<ImportState>()
const importFile = ref<HTMLInputElement>()
const importStale = ref(false)
const importNotice = ref("")
/// The known stores of other local clients, shown by Discover; absent until
/// the user asks. A row carries no entry and no value.
const discovery = ref<McpImportSource[]>()
const editorForm = ref<HTMLFormElement>()
const search = ref("")
let mounted = true
const USER_SCOPE = "__user__"
const dirty = computed(() => draft.value !== undefined && JSON.stringify(draft.value) !== baseline.value)
const oauthEnabled = computed(() => draft.value !== undefined && (editing.value ? draft.value.oauthAction === "set" || (draft.value.oauthAction === "keep" && draft.value.oauth) : draft.value.oauth))
const staticCredentialWithheld = computed(() => draft.value !== undefined && draft.value.secretsInUserScope && draft.value.authConfigured)
const entries = computed(() => catalog.value?.servers.filter((server) => server.id.toLocaleLowerCase().includes(search.value.toLocaleLowerCase())) ?? [])
const invalidEntries = computed(() => catalog.value?.invalid.filter((entry) => entry.id.toLocaleLowerCase().includes(search.value.toLocaleLowerCase())) ?? [])
const scopeItems = computed(() => [
  { value: USER_SCOPE, label: s('User configuration', '用户配置') },
  ...sessions.workspaces.map((workspace) => ({ value: workspace.cwd, label: `${workspace.name} — ${workspace.cwd}` })),
])
const importActionItems = computed(() => [
  { value: "import", label: s('Import', '导入') },
  { value: "skip", label: s('Skip', '跳过') },
  { value: "replace", label: s('Replace the existing entry', '替换现有条目') },
  { value: "rename", label: s('Import under a new id', '以新 ID 导入') },
])
const sourceItems = computed(() => [
  { value: "user", label: s('User', '用户') },
  ...(loadedCwd.value ? [{ value: "project", label: s('Project', '项目') }] : []),
])
const transportItems = [
  { value: "stdio", label: "stdio" },
  { value: "http", label: "HTTP" },
  { value: "sse", label: "SSE" },
]
const argsActionItems = computed(() => [
  { value: "keep", label: s('Keep stored arguments', '保留已存参数') },
  { value: "replace", label: s('Replace all arguments', '替换全部参数') },
])
const oauthActionItems = computed(() => [
  { value: "keep", label: s('Preserve existing configuration', '保留现有配置') },
  { value: "set", label: s('Configure / edit OAuth', '配置 / 编辑 OAuth') },
  { value: "remove", label: s('Remove explicit configuration (automatic discovery remains)', '移除显式配置（仍可自动发现）') },
])
const metadataActionItems = computed(() => [
  { value: "keep", label: s('Keep', '保留') },
  { value: "set", label: s('Replace', '替换') },
  { value: "remove", label: s('Remove', '移除') },
])
const bearerActionItems = computed(() => [
  { value: "keep", label: s('Keep stored token', '保留已存储的令牌') },
  { value: "set", label: s('Replace token', '替换令牌'), disabled: oauthEnabled.value },
  { value: "remove", label: s('Remove token', '移除令牌') },
])
function secretActionItems(stored: boolean | undefined) {
  return [
    ...(stored ? [{ value: "keep", label: s('Keep', '保留') }] : []),
    { value: "set", label: s('Set', '设置') },
    ...(stored ? [{ value: "remove", label: s('Remove', '删除') }] : []),
  ]
}
bindSettingsDirty(computed(() => dirty.value || !!clientSecret.value || !!callback.value))
watch(draft, () => { testResult.value = undefined }, { deep: true })

function reasonText(reason: string | undefined): string {
  switch (reason) {
    case "timeout": return s('The server did not answer before the deadline.', '服务器在时限内未响应。')
    case "response": return s('The server answered something Dim cannot use.', '服务器的返回无法被 Dim 使用。')
    case "cleanup": return s('The temporary connection did not close cleanly.', '临时连接未能正常关闭。')
    default: return s('The connection or a request failed.', '连接或请求失败。')
  }
}
function scope(): Record<string, unknown> { return loadedCwd.value ? { cwd: loadedCwd.value } : {} }
function target(server: McpEntry): Record<string, unknown> { return { ...scope(), id: server.id, source: server.source } }
function revision(source: "user" | "project"): string {
  const value = source === "user" ? catalog.value?.userRevision : catalog.value?.projectRevision
  if (value === undefined) throw new Error(s('Load the project catalog before editing it.', '编辑项目前请先加载项目目录。'))
  return value
}
async function read(): Promise<void> { catalog.value = await connection.client.call("mcp.catalog", scope(), readMcp) }
async function refreshDetail(): Promise<void> {
  if (!detail.value) return
  const server = catalog.value?.servers.find((entry) => entry.id === detail.value?.server.id && entry.source === detail.value.server.source)
  if (!server) { detail.value = undefined; return }
  detail.value = { server, inspection: await connection.client.call("mcp.inspect", target(server), readMcpInspection) }
}
async function inspect(server: McpEntry): Promise<void> {
  if (busy.value) return
  if (clientSecret.value && !window.confirm(s('Discard the entered OAuth client secret?', '放弃已输入的 OAuth 客户端密钥？'))) return
  busy.value = true
  error.value = ""
  try {
    detail.value = { server, inspection: await connection.client.call("mcp.inspect", target(server), readMcpInspection) }
    clientSecret.value = ""
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function load(path = loadedCwd.value): Promise<void> {
  if (busy.value) return
  if ((dirty.value || clientSecret.value || callback.value || authorization.value) && !window.confirm(s('Discard unsaved MCP configuration and authentication?', '放弃未保存的 MCP 配置和认证？'))) return
  busy.value = true
  error.value = ""
  try {
    if (!connection.supports("mcp.catalog")) throw new Error(s('MCP management is not available on this server.', '此服务器不支持 MCP 管理。'))
    const next = await connection.client.call("mcp.catalog", path ? { cwd: path } : {}, readMcp)
    if (authorization.value) {
      await connection.client.call("mcp.auth.cancel", { attemptId: authorization.value.attemptId }, readAck)
      authorization.value = undefined
    }
    loadedCwd.value = path
    catalog.value = next
    draft.value = undefined
    detail.value = undefined
    clientSecret.value = ""
    callback.value = ""
    testResult.value = undefined
    importPreview.value = undefined
    importStale.value = false
    importNotice.value = ""
    discovery.value = undefined
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
function chooseImportFile(): void {
  if (!busy.value) importFile.value?.click()
}
async function importFileChosen(event: Event): Promise<void> {
  const input = event.target as HTMLInputElement
  const file = input.files?.[0]
  input.value = ""
  if (!file || busy.value) return
  busy.value = true
  error.value = ""
  importNotice.value = ""
  try { discovery.value = undefined; await readImportPreview({ document: await file.text() }) }
  catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
/// The known stores of other local clients, as the owner reports them. The
/// page never asks for a document and never watches the files.
async function discover(): Promise<void> {
  if (busy.value) return
  busy.value = true
  error.value = ""
  importNotice.value = ""
  try {
    discovery.value = await connection.client.call("mcp.import.sources", scope(), readMcpImportSources)
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
function closeDiscovery(): void {
  if (!busy.value) discovery.value = undefined
}
/// Previews one ready store by id: the owner reads it and answers the same
/// entries a file preview would; its text is never sent to this page.
async function previewDiscovered(source: McpImportSource): Promise<void> {
  if (busy.value || source.state !== "ready") return
  busy.value = true
  error.value = ""
  importNotice.value = ""
  try {
    await readImportPreview({ from: source.id })
    discovery.value = undefined
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
/// The source an import writes: the project when the selector names a
/// workspace, otherwise the user source.
function importSource(): "user" | "project" { return loadedCwd.value ? "project" : "user" }
/// The destination the file will be imported into, named before a file is
/// chosen.
function importDestination(): string {
  return importSource() === "project"
    ? s(`Import into: project ${loadedCwd.value}`, `导入到：项目 ${loadedCwd.value}`)
    : s('Import into: user configuration', '导入到：用户配置')
}
async function readImportPreview(input: { document?: string; from?: string }): Promise<void> {
  const source = importSource()
  const params: Record<string, unknown> = { ...scope(), source }
  if (input.document !== undefined) params.document = input.document
  if (input.from !== undefined) params.from = input.from
  const preview = await connection.client.call("mcp.import.preview", params, readMcpImportPreview)
  importPreview.value = {
    source,
    document: input.document,
    from: input.from,
    store: preview.store,
    sourceRevision: preview.sourceRevision,
    revision: preview.revision,
    unknownTopLevel: preview.unknownTopLevel,
    entries: preview.entries.map((entry) => ({
      ...entry,
      action: entry.status === "changed" || entry.status === "identical" ? "import" : "skip",
      newId: "",
    })),
  }
  importStale.value = false
  error.value = ""
}
async function previewImportAgain(): Promise<void> {
  const preview = importPreview.value
  if (!preview || busy.value) return
  busy.value = true
  try {
    if (preview.from !== undefined) await readImportPreview({ from: preview.from })
    else if (preview.document !== undefined) await readImportPreview({ document: preview.document })
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
function closeImportPreview(): void {
  if (!busy.value) importPreview.value = undefined
}
const importReady = computed(() => {
  const preview = importPreview.value
  if (!preview || !preview.entries.length) return false
  return preview.entries.every((entry) => entry.action !== "rename" || !!entry.newId.trim())
})
async function applyImport(): Promise<void> {
  const preview = importPreview.value
  if (!preview || busy.value || !importReady.value) return
  busy.value = true
  error.value = ""
  importStale.value = false
  try {
    const resolutions: McpImportResolution[] = preview.entries.map((entry) => ({
      id: entry.id,
      action: entry.action,
      ...(entry.action === "rename" ? { newId: entry.newId.trim() } : {}),
    }))
    const params: Record<string, unknown> = { ...scope(), source: preview.source, expectedVersion: preview.revision, resolutions }
    if (preview.from !== undefined) {
      params.from = preview.from
      params.expectedSourceRevision = preview.sourceRevision
    } else params.document = preview.document
    await connection.client.call("mcp.import.apply", params, readMcpImportApplied)
    importPreview.value = undefined
    await read()
    importNotice.value = importSummary(preview)
  } catch (cause) {
    if (cause instanceof RpcError && cause.appError === "mcp_settings_conflict") {
      importStale.value = true
      error.value = s('The MCP source changed since this preview; no entry was imported, and an entry it would have replaced may now be disabled. Preview again before applying.', '自本次预览后 MCP 源已更改；未导入任何条目，它原本会替换的条目现在可能已禁用。请重新预览后再应用。')
    } else error.value = messageFrom(cause)
  } finally { busy.value = false }
}
function importSummary(preview: ImportState): string {
  const counts = { imported: 0, replaced: 0, renamed: 0, skipped: 0, unchanged: 0 }
  for (const entry of preview.entries) {
    if (entry.action === "skip") counts.skipped += 1
    else if (entry.action === "replace") counts.replaced += 1
    else if (entry.action === "rename") counts.renamed += 1
    else if (entry.status === "identical") counts.unchanged += 1
    else counts.imported += 1
  }
  return s(
    `Imported ${counts.imported}, replaced ${counts.replaced}, renamed ${counts.renamed}, skipped ${counts.skipped}, unchanged ${counts.unchanged}. Imported entries start disabled: enable one with Enable, and enable project entries locally.`,
    `已导入 ${counts.imported} 项，替换 ${counts.replaced} 项，重命名 ${counts.renamed} 项，跳过 ${counts.skipped} 项，未改变 ${counts.unchanged} 项。导入的条目默认禁用：请用“启用”开启，项目条目还需在本地启用。`,
  )
}
function importChanged(entry: McpImportEntry): string {
  return entry.changes.map((field) => `${field.field} (${field.kind})`).join(", ")
}
function importUnsupported(entry: McpImportEntry): string {
  return entry.unsupported.map((field) => `${field.field} (${field.effect === "kept-unused" ? s('kept unused', '保留未用') : s('dropped', '丢弃')})`).join(", ")
}
function importOmitted(entry: McpImportEntry): string {
  return s(`${entry.omitted} more field names are not listed.`, `另有 ${entry.omitted} 个字段名未列出。`)
}
function importUnknown(): string {
  const names = importPreview.value?.unknownTopLevel ?? []
  return names.length
    ? s('Top-level members the import does not carry: ', '导入不会携带的顶层成员：') + names.join(", ") + "."
    : s('Top-level members the import does not carry: none.', '导入不会携带的顶层成员：无。')
}
function create(): void {
  if (dirty.value && !window.confirm(s('Discard unsaved MCP configuration?', '放弃未保存的 MCP 配置？'))) return
  draft.value = { id: "", source: "user", type: "stdio", command: "", args: [], argsAction: "replace", storedArgsCount: 0, cwd: "", url: "", secrets: [], enabled: false, oauth: false, clientId: "", token: "", bearerAction: "keep", authConfigured: false, oauthAction: "keep", callbackPort: "", scopes: "", metadataUrl: "", metadataConfigured: false, metadataAction: "keep", secretsInUserScope: false, timeout: "" }
  baseline.value = JSON.stringify(draft.value)
  editing.value = false
  editorRevision.value = revision("user")
}
async function edit(server: McpEntry): Promise<void> {
  if (server.source === "plugin" || busy.value) return
  if (dirty.value && !window.confirm(s('Discard unsaved MCP configuration?', '放弃未保存的 MCP 配置？'))) return
  busy.value = true
  error.value = ""
  try {
    const entry = await connection.client.call("mcp.configuration", target(server), readMcpEditableEntry)
    if (entry.revision === undefined) throw new Error(s('This entry has no source revision and cannot be edited.', '此条目没有源修订号，无法编辑。'))
    const config = entry.configuration
    draft.value = {
      id: server.id, source: server.source, type: config.type,
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
    editing.value = true
    editorRevision.value = entry.revision
    baseline.value = JSON.stringify(draft.value)
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
function closeEditor(): void {
  if (dirty.value && !window.confirm(s('Discard unsaved MCP configuration?', '放弃未保存的 MCP 配置？'))) return
  draft.value = undefined
}
function validDraft(form: Draft): boolean {
  const names = form.secrets.map((secret) => secret.name.trim())
  if (names.some((name) => !name) || new Set(names).size !== names.length) {
    error.value = s('Secret field names must be nonempty and unique.', '机密字段名称不能为空且必须唯一。')
    return false
  }
  // A reference is expanded by the daemon, which validates the expanded
  // endpoint; only a literal endpoint is checked here.
  const url = form.url.trim()
  if (form.type !== "stdio" && !form.secretsInUserScope && url && !url.includes("${")) {
    try { new URL(url) } catch {
      error.value = s('The endpoint must be an absolute URL, or contain a variable reference the daemon expands.', '端点必须是绝对 URL，或包含由守护进程展开的变量引用。')
      return false
    }
  }
  return editorForm.value?.reportValidity() ?? false
}
async function save(): Promise<void> {
  const form = draft.value
  if (!form || busy.value || !dirty.value) return
  error.value = ""
  if (!validDraft(form)) return
  if (!editing.value && catalog.value?.servers.some((server) => server.id === form.id.trim())) { error.value = s('This ID already exists in the effective catalog. Edit the existing entry instead.', '此 ID 已存在于有效目录中。请编辑现有条目。'); return }
  if (!editing.value && form.source === "user" && form.enabled && !window.confirm(s('Enabling this server permits its configured command or network connection to run. Continue?', '启用此服务器将允许运行其配置命令或网络连接。是否继续？'))) return
  busy.value = true
  try {
    if (editing.value) {
      await connection.client.call("mcp.update", { ...scope(), id: form.id, source: form.source, expectedVersion: editorRevision.value, patch: patch(form) }, readAck)
    } else {
      await connection.client.call("mcp.save", { ...scope(), id: form.id.trim(), source: form.source, expectedVersion: revision(form.source), configuration: configuration(form) }, readAck)
    }
    draft.value = undefined
    await read()
    await refreshDetail()
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function testServer(server?: McpEntry): Promise<void> {
  if (busy.value) return
  error.value = ""
  let params: ReturnType<typeof target> | { id: string; source: "user" | "project"; cwd?: string; patch?: McpConfigurationPatch; configuration?: McpServerInput }
  let id: string
  if (server) {
    params = target(server)
    id = server.id
  } else {
    const form = draft.value
    if (!form || !validDraft(form)) return
    params = { ...scope(), id: form.id.trim(), source: form.source, ...(editing.value ? { patch: patch(form) } : { configuration: configuration(form) }) }
    id = form.id
  }
  if (!window.confirm(s('Open a temporary connection and list tools? This executes the configured command or contacts its endpoint. No configuration is saved and the connection is closed after the test.', '打开临时连接并列出工具？这会执行配置的命令或连接端点。测试不会保存设置，且会在结束后关闭连接。'))) return
  busy.value = true
  testResult.value = undefined
  try {
    const probe = await connection.client.call("mcp.test", params, readMcpTest)
    testResult.value = { id, probe }
    await read()
    await refreshDetail()
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function action(method: string, server: McpEntry): Promise<void> {
  if (busy.value) return
  if (method === "mcp.remove" && !window.confirm(s('Remove this server configuration?', '移除此服务器配置？'))) return
  if (method === "mcp.set_enabled" && !server.enabled && !window.confirm(s('Enabling this server permits its configured command or network connection to run. Continue?', '启用此服务器将允许运行其配置命令或网络连接。是否继续？'))) return
  busy.value = true
  error.value = ""
  try {
    const params = target(server)
    if (method === "mcp.set_enabled" || method === "mcp.remove") {
      if (server.source === "plugin") return
      params.expectedVersion = revision(server.source)
      if (method === "mcp.set_enabled") params.enabled = !server.enabled
    }
    if (method === "mcp.auth.start") {
      if (detail.value?.server.id === server.id && detail.value.server.source === server.source && clientSecret.value) params.clientSecret = clientSecret.value
      const attempt = await connection.client.call(method, params, readMcpAuthStart)
      if (!mounted) {
        await connection.client.call("mcp.auth.cancel", { attemptId: attempt.attemptId }, readAck)
        return
      }
      authorization.value = attempt
      authorizingId.value = server.id
    }
    else await connection.client.call(method, params, readAck)
    await read()
    await refreshDetail()
  } catch (cause) { error.value = messageFrom(cause) }
  finally { if (method === "mcp.auth.start") clientSecret.value = ""; busy.value = false }
}
async function removeInvalid(entry: McpInvalidEntry): Promise<void> {
  if (busy.value || !!draft.value || entry.source === "plugin") return
  if (!window.confirm(s('Remove this server configuration?', '移除此服务器配置？'))) return
  busy.value = true
  error.value = ""
  try {
    await connection.client.call("mcp.remove", { ...scope(), id: entry.id, source: entry.source, expectedVersion: revision(entry.source) }, readAck)
    await read()
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function move(server: McpEntry): Promise<void> {
  if (busy.value || server.source === "plugin" || !connection.supports("mcp.move")) return
  const from = server.source === "user" ? "user" as const : "project" as const
  const to = from === "user" ? "project" as const : "user" as const
  if (to === "project" && !loadedCwd.value) { error.value = s('Choose a workspace before moving a server into its project configuration.', '移入项目配置前请先选择工作区。'); return }
  if (dirty.value && !window.confirm(s('Discard unsaved MCP configuration?', '放弃未保存的 MCP 配置？'))) return
  const message = to === "project"
    ? s('Move this server to the project? Withheld values stay in your local Dim configuration, the workspace file gets only the public configuration, and the entry starts disabled.', '将此服务器移入项目？隐藏值保留在本地 Dim 配置中，工作区文件只包含公开配置，且条目默认禁用。')
    : s('Move this server to user scope? It keeps its current enabled state and its withheld values move to your local configuration.', '将此服务器移入用户范围？它会保留当前启用状态，隐藏值会移至本地配置。')
  if (!window.confirm(message)) return
  const user = catalog.value?.userRevision
  const project = catalog.value?.projectRevision
  const expectedFromVersion = from === "user" ? user : project
  const expectedToVersion = to === "user" ? user : project
  if (expectedFromVersion === undefined || expectedToVersion === undefined) { error.value = s('Load the catalog before moving this server.', '移动此服务器前请先加载目录。'); return }
  busy.value = true
  error.value = ""
  try {
    await connection.client.call("mcp.move", { ...scope(), id: server.id, from, to, expectedFromVersion, expectedToVersion }, readAck)
    draft.value = undefined
    detail.value = undefined
    await read()
    await refreshDetail()
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
function moveToUser(id: string): void {
  const server = catalog.value?.servers.find((entry) => entry.id === id && entry.source === "project")
  if (server) void move(server)
}
async function finishAuth(cancel = false): Promise<void> {
  if (!authorization.value || (!cancel && !callback.value) || busy.value) return
  busy.value = true
  error.value = ""
  try {
    await connection.client.call(cancel ? "mcp.auth.cancel" : "mcp.auth.complete", { attemptId: authorization.value.attemptId, ...(!cancel ? { callbackUrl: callback.value } : {}) }, readAck)
    callback.value = ""
    authorization.value = undefined
    await read()
    await refreshDetail()
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function changeScope(value: unknown): Promise<void> {
  const path = typeof value === "string" && value !== USER_SCOPE ? value : ""
  await load(path)
}
reloadSettingsOnReconnect(connection, () => load(), computed(() => draft.value !== undefined || !!clientSecret.value || !!callback.value || !!authorization.value))
onMounted(() => load())
onBeforeUnmount(() => {
  mounted = false
  callback.value = ""
  clientSecret.value = ""
  if (authorization.value) void connection.client.call("mcp.auth.cancel", { attemptId: authorization.value.attemptId }, readAck).catch(() => undefined)
})
</script>

<template>
  <article class="settings-page">
    <header><h2 data-route-heading tabindex="-1">MCP</h2><p>{{ s('Manage command and remote MCP servers. Project servers stay disabled until you explicitly enable them locally.', '管理命令和远程 MCP 服务器。项目服务器默认禁用，只有您在本地明确启用后才会运行。') }}</p></header>
    <form class="settings-card settings-form" @submit.prevent="load()">
      <UFormField :label="s('Configuration scope', '配置范围')">
        <USelect :model-value="loadedCwd || USER_SCOPE" :items="scopeItems" :disabled="busy" @update:model-value="changeScope" />
      </UFormField>
      <p>{{ s('Choose a workspace to include its project MCP servers alongside user and plugin servers. This does not change a server\'s working directory.', '选择工作区后，会同时显示该项目、用户和插件提供的 MCP 服务器。这不会更改服务器的运行目录。') }}</p>
      <div class="settings-actions">
        <UButton type="submit" color="neutral" variant="soft" :disabled="busy">{{ s('Load / refresh', '加载 / 刷新') }}</UButton>
        <UButton color="primary" :disabled="busy || !catalog || !connection.supports('mcp.save')" @click="create">{{ s('Add server', '添加服务器') }}</UButton>
        <input ref="importFile" type="file" accept="application/json,.json" hidden :disabled="busy" @change="importFileChosen" />
        <UButton color="neutral" variant="soft" :disabled="busy || !!draft || !connection.supports('mcp.import.preview')" @click="chooseImportFile">{{ s('Import from file…', '从文件导入…') }}</UButton>
        <UButton color="neutral" variant="soft" :disabled="busy || !!draft || !connection.supports('mcp.import.sources')" @click="discover">{{ s('Discover…', '发现…') }}</UButton>
        <span class="settings-meta">{{ importDestination() }}</span>
      </div>
    </form>
    <GeneralSettingsStatus :busy="busy" :error="error" @reload="load()" />
    <UAlert v-if="importNotice" class="settings-banner" color="success" variant="soft" role="status" :description="importNotice" />
    <section v-if="discovery" class="settings-card" aria-live="polite">
      <h3>{{ s('MCP stores of other local clients', '其他本地客户端的 MCP 存储') }}</h3>
      <p>{{ s('These files belong to other clients. Dim reads them only when you open Discover or preview one, inside the daemon, and never shows or returns a value from them.', '这些文件属于其他客户端。只有您打开“发现”或预览其中一项时，Dim 才会在守护进程内读取这些文件，且绝不显示或返回其中的任何值。') }}</p>
      <div v-for="row in discovery" :key="row.id" class="runtime-detail">
        <p><strong>{{ row.id }}</strong> · {{ row.client }} · {{ row.scope }} · {{ row.state }}</p>
        <p>{{ row.path }}</p>
        <p v-if="row.state === 'ready'">{{ s('Entries', '条目') }}: {{ row.entries }}</p>
        <p v-else-if="row.error">{{ row.error }}</p>
        <UButton v-if="row.state === 'ready'" size="sm" color="neutral" variant="soft" :disabled="busy" @click="previewDiscovered(row)">{{ s('Preview', '预览') }}</UButton>
      </div>
      <p v-if="!discovery.length" role="status">{{ s('No known store was found.', '未找到已知存储。') }}</p>
      <div class="settings-actions"><UButton color="neutral" variant="soft" :disabled="busy" @click="closeDiscovery">{{ s('Close discovery', '关闭发现') }}</UButton></div>
    </section>
    <section v-if="importPreview" class="settings-card" aria-live="polite">
      <h3>{{ s('Import preview', '导入预览') }} · {{ importPreview.source }} · {{ importPreview.revision }}</h3>
      <p v-if="importPreview.from">{{ s('From store', '来自存储') }} {{ importPreview.from }}<template v-if="importPreview.store"> · {{ importPreview.store.client }} · {{ importPreview.store.scope }} · {{ importPreview.store.path }}</template> · {{ s('its text stays in the daemon.', '其内容留在守护进程中。') }}</p>
      <p>{{ s('Each entry is matched against the selected source. No value is shown: changed fields are named only. Imported and replaced entries start disabled.', '每个条目会与所选来源比对。此处不显示任何值，只列出变更字段名。导入和替换的条目默认禁用。') }}</p>
      <p v-if="importPreview.source === 'project'">{{ s('The workspace file receives each entry\'s public skeleton: the command, declared working directory, URL origin and public OAuth fields. Arguments, environment and header values, the bearer token, a URL with userinfo, path, query or fragment, hidden OAuth fields and unknown members go to your user-scope mcp-secrets record, never to the workspace file.', '工作区文件只写入每个条目的公开骨架：命令、声明的运行目录、URL 来源和公开 OAuth 字段。参数、环境变量与请求头的值、Bearer 令牌、带用户信息、路径、查询或片段的 URL、隐藏的 OAuth 字段及未知成员会写入您的用户范围 mcp-secrets 记录，绝不会写入工作区文件。') }}</p>
      <p>{{ importUnknown() }}</p>
      <UAlert v-if="importStale" class="settings-banner" color="warning" variant="soft" :description="s('This preview is older than the source.', '此预览早于当前源。')">
        <template #actions><UButton size="sm" color="neutral" variant="soft" :disabled="busy" @click="previewImportAgain">{{ s('Preview again', '重新预览') }}</UButton></template>
      </UAlert>
      <p v-if="!importPreview.entries.length" role="status">{{ s('The document holds no mcpServers entries.', '文档中没有 mcpServers 条目。') }}</p>
      <div v-for="entry in importPreview.entries" :key="entry.id" class="runtime-detail">
        <h4>{{ entry.id }} · {{ entry.status }}<template v-if="entry.transport"> · {{ entry.transport }}</template></h4>
        <p v-if="entry.changes.length">{{ s('Changed fields', '变更字段') }}: {{ importChanged(entry) }}</p>
        <p v-if="entry.unsupported.length">{{ s('Not carried', '不携带') }}: {{ importUnsupported(entry) }}</p>
        <p v-if="entry.omitted">{{ importOmitted(entry) }}</p>
        <p v-if="entry.error">{{ entry.error }}</p>
        <p v-if="entry.status === 'invalid'">{{ s('This entry does not decode, so it can only be skipped.', '此条目无法解码，只能跳过。') }}</p>
        <UFormField v-else :label="s('Action', '操作')">
          <USelect v-model="entry.action" :items="importActionItems" :disabled="busy" :aria-label="s('Import action', '导入操作')" />
        </UFormField>
        <UFormField v-if="entry.action === 'rename'" :label="s('New id', '新 ID')">
          <UInput v-model="entry.newId" :disabled="busy" :aria-label="s('New id for this entry', '此条目的新 ID')" required />
        </UFormField>
      </div>
      <div class="settings-actions">
        <UButton color="primary" :disabled="busy || !importReady" @click="applyImport">{{ s('Apply import', '应用导入') }}</UButton>
        <UButton color="neutral" variant="soft" :disabled="busy" @click="closeImportPreview">{{ s('Close preview', '关闭预览') }}</UButton>
      </div>
    </section>
    <UAlert v-for="issue in catalog?.errors" :key="issue" class="settings-banner" color="warning" variant="soft" :description="issue" />
    <section v-if="invalidEntries.length" class="settings-card">
      <h3>{{ s('Entries that did not decode', '无法解码的条目') }}</h3>
      <p>{{ s('These entries stay in their file as written. They cannot run, and they can only be removed or replaced.', '这些条目按原样保留在文件中。它们无法运行，只能被移除或替换。') }}</p>
      <div v-for="entry in invalidEntries" :key="`${entry.source}:${entry.id}`" class="runtime-detail">
        <h4>{{ entry.id }} · {{ entry.source }}</h4>
        <p>{{ entry.error }}</p>
        <div class="settings-actions"><UButton color="error" variant="ghost" :disabled="busy || !!draft || entry.source === 'plugin' || !connection.supports('mcp.remove')" @click="removeInvalid(entry)">{{ s('Remove', '移除') }}</UButton></div>
      </div>
    </section>
    <section v-if="detail" class="settings-card settings-form" aria-live="polite">
      <h3>{{ s('Server details', '服务器详情') }}: {{ detail.server.id }}</h3>
      <p>{{ detail.server.source }} · {{ detail.server.transport }} · {{ detail.inspection.enabled ? s('Enabled', '已启用') : s('Disabled', '已禁用') }}</p>
      <template v-if="detail.inspection.configuration.type === 'stdio'"><p>{{ s('Executable', '可执行程序') }}: <code>{{ detail.inspection.configuration.command }}</code></p><p>{{ s('Stored arguments (hidden)', '已存参数（隐藏）') }}: {{ detail.inspection.configuration.argsCount }}</p><p>{{ s('Working directory', '工作目录') }}: {{ detail.inspection.configuration.cwd || s('Daemon directory', '守护进程目录') }}</p><p>{{ s('Environment names', '环境变量名') }}: {{ detail.inspection.configuration.envNames.join(', ') || '—' }}</p><p>{{ s('Variables', '变量') }}: {{ detail.inspection.configuration.referencedVariables.join(', ') || '—' }}</p><p>{{ s('Unset variables', '未设置变量') }}: {{ detail.inspection.configuration.unsetVariables.join(', ') || '—' }}</p></template>
      <template v-else><p>{{ s('Endpoint (origin only)', '端点（仅来源）') }}: <code>{{ detail.inspection.configuration.endpoint }}</code></p><p>{{ s('Header names', '请求头名称') }}: {{ detail.inspection.configuration.headerNames.join(', ') || '—' }}</p><p>{{ s('Stored bearer token', '已存储的 Bearer 令牌') }}: {{ detail.inspection.configuration.authConfigured ? s('Configured', '已配置') : s('None', '无') }}</p><p v-if="detail.inspection.configuration.oauth?.clientId">OAuth client ID: {{ detail.inspection.configuration.oauth.clientId }}</p><p>{{ s('Variables', '变量') }}: {{ detail.inspection.configuration.referencedVariables.join(', ') || '—' }}</p><p>{{ s('Unset variables', '未设置变量') }}: {{ detail.inspection.configuration.unsetVariables.join(', ') || '—' }}</p></template>
      <p v-if="detail.inspection.configuration.timeout !== undefined">{{ s('Tool call timeout', '工具调用超时') }}: {{ detail.inspection.configuration.timeout }} ms</p>
      <p>{{ s('Connection snapshots do not open a connection. Tool lists are the last successful discovery for each live connection generation.', '查看快照不会打开连接。工具列表来自每个活动连接代次最近一次成功的发现。') }}</p>
      <p v-if="!detail.inspection.connections.length" role="status">{{ s('No live session connections. Use Test connection to discover tools without creating a session.', '没有活动会话连接。可通过“测试连接”发现工具，无需创建会话。') }}</p>
      <section v-for="(runtime, index) in detail.inspection.connections" :key="index" class="runtime-detail"><h4>{{ s('Connection', '连接') }} {{ index + 1 }} · {{ runtime.state }}<template v-if="runtime.expired"> · {{ s('Credential expired', '凭据已过期') }}</template></h4><ul v-if="runtime.tools"><li v-for="tool in runtime.tools" :key="tool.name"><strong>{{ tool.name }}</strong><p v-if="tool.description">{{ tool.description }}</p></li></ul><p v-if="runtime.tools === undefined">{{ s('Tools have not been discovered.', '尚未发现工具。') }}</p><p v-else-if="!runtime.tools.length">{{ s('The server exposes no tools.', '服务器没有提供工具。') }}</p></section>
      <UFormField v-if="detail.inspection.configuration.type !== 'stdio' && detail.inspection.configuration.oauth?.clientId" :label="s('OAuth client secret (optional, sent once on Sign in)', 'OAuth 客户端密钥（可选，仅在登录时发送一次）')">
        <UInput v-model="clientSecret" type="password" autocomplete="new-password" :disabled="busy" />
      </UFormField>
      <div class="settings-actions">
        <UButton color="neutral" variant="soft" :disabled="busy" @click="inspect(detail.server)">{{ s('Refresh details', '刷新详情') }}</UButton>
        <UButton color="neutral" variant="soft" :disabled="busy || !connection.supports('mcp.test') || (detail.server.source !== 'user' && !detail.server.enabled)" @click="testServer(detail.server)">{{ s('Test connection', '测试连接') }}</UButton>
        <UButton v-if="detail.server.transport !== 'stdio'" color="neutral" variant="soft" :disabled="busy || !!authorization || !detail.server.enabled || !connection.supports('mcp.auth.start')" @click="action('mcp.auth.start', detail.server)">{{ s('Sign in', '登录') }}</UButton>
      </div>
    </section>
    <form v-if="draft" ref="editorForm" class="settings-card settings-form" autocomplete="off" @submit.prevent="save">
      <h3>{{ editing ? s('Edit server', '编辑服务器') : s('New server', '新服务器') }}</h3>
      <UAlert v-if="editing && draft.secretsInUserScope" class="settings-banner" color="warning" variant="soft" :description="s('Withheld arguments, environment, headers, URL parts and OAuth metadata are stored in your user-scope configuration. They cannot be edited here; non-secret fields still save through the owner.', '隐藏的参数、环境变量、请求头、URL 部分和 OAuth 元数据存储在用户范围配置中。此处无法编辑；非机密字段仍可通过所有者保存。')">
        <template #actions><UButton size="sm" color="neutral" variant="soft" :disabled="busy || !connection.supports('mcp.move')" @click="moveToUser(draft.id)">{{ s('Move to user to edit', '移到用户范围编辑') }}</UButton></template>
      </UAlert>
      <fieldset :disabled="busy">
        <UFormField :label="s('Server ID', '服务器 ID')"><UInput v-model="draft.id" :disabled="editing" required /></UFormField>
        <UFormField :label="s('Source', '来源')"><USelect v-model="draft.source" :items="sourceItems" :disabled="editing" /></UFormField>
        <p v-if="editing">{{ s('The source and transport are fixed. Changes write only this source. Use Move to change the source; a move into a project keeps withheld values in your user-scope configuration.', '来源和传输方式固定。更改只写入此来源。使用“移动”更改来源；移入项目时隐藏值保留在用户范围配置中。') }}</p>
        <UFormField :label="s('Transport', '传输方式')"><USelect v-model="draft.type" :items="transportItems" :disabled="editing" /></UFormField>
        <UFormField :label="s('Tool call timeout (optional, milliseconds)', '工具调用超时（可选，毫秒）')"><UInput :model-value="String(draft.timeout ?? '')" type="number" min="1000" max="86400000" step="1" placeholder="600000" @update:model-value="draft.timeout = $event ?? ''" /></UFormField>
        <template v-if="draft.type === 'stdio'">
          <UFormField :label="s('Executable', '可执行程序')"><UInput v-model="draft.command" required /></UFormField>
          <UFormField :label="s('Working directory (optional)', '工作目录（可选）')"><UInput v-model="draft.cwd" /></UFormField>
          <UFormField v-if="editing && !draft.secretsInUserScope" :label="`${s('Stored arguments (hidden)', '已存参数（隐藏）')}: ${draft.storedArgsCount}`">
            <USelect v-model="draft.argsAction" :items="argsActionItems" />
          </UFormField>
          <div v-if="!draft.secretsInUserScope && (!editing || draft.argsAction === 'replace')">
            <strong>{{ s('Arguments (one per field)', '参数（每个字段一个）') }}</strong>
            <div v-for="(_, index) in draft.args" :key="index" class="argument-row">
              <UInput v-model="draft.args[index]" type="password" autocomplete="new-password" :aria-label="s('Argument', '参数')" />
              <UButton color="neutral" variant="soft" @click="draft.args.splice(index, 1)">{{ s('Remove', '移除') }}</UButton>
            </div>
            <UButton size="sm" color="neutral" variant="soft" @click="draft.args.push('')">{{ s('Add argument', '添加参数') }}</UButton>
          </div>
        </template>
        <template v-else>
          <p v-if="editing">{{ s('Current endpoint', '当前端点') }}: {{ draft.endpoint }} — {{ s('Path and credentials are hidden.', '路径和凭据已隐藏。') }}</p>
          <UFormField v-if="!draft.secretsInUserScope" :label="editing ? s('Replace URL (blank preserves the stored URL)', '替换 URL（留空保留已存储的 URL）') : 'URL'">
            <UInput v-model="draft.url" type="text" inputmode="url" :required="!editing" autocomplete="off" />
          </UFormField>
          <template v-if="!editing">
            <UCheckbox v-model="draft.oauth" :disabled="!!draft.token" :label="s('Configure OAuth', '配置 OAuth')" />
            <UFormField v-if="!draft.oauth" :label="s('Bearer token (optional, write-only)', 'Bearer 令牌（可选，只写）')">
              <UInput v-model="draft.token" type="password" autocomplete="new-password" />
            </UFormField>
          </template>
          <UFormField v-if="editing && !staticCredentialWithheld" :label="s('OAuth configuration', 'OAuth 配置')">
            <USelect v-model="draft.oauthAction" :items="oauthActionItems" />
          </UFormField>
          <section v-if="(!editing && draft.oauth) || (editing && draft.oauthAction === 'set' && !staticCredentialWithheld)" class="settings-form">
            <p>{{ s('OAuth cannot be combined with a bearer token or Authorization header. Remove those explicitly when enabling OAuth. Leave optional fields blank to use defaults.', 'OAuth 不能与 Bearer 令牌或 Authorization 请求头一起使用。启用 OAuth 时请明确移除这些字段。可选字段留空使用默认值。') }}</p>
            <UFormField :label="s('OAuth client ID (optional)', 'OAuth 客户端 ID（可选）')"><UInput v-model="draft.clientId" /></UFormField>
            <UFormField :label="s('Callback port (default 1456)', '回调端口（默认 1456）')"><UInput v-model="draft.callbackPort" type="number" min="1" max="65535" step="1" placeholder="1456" /></UFormField>
            <UFormField :label="s('Scopes (space separated, optional)', '权限范围（空格分隔，可选）')"><UInput v-model="draft.scopes" /></UFormField>
            <UFormField v-if="editing && !draft.secretsInUserScope" :label="`${s('Authorization metadata URL', '授权元数据 URL')} · ${draft.metadataConfigured ? s('Configured (hidden)', '已配置（隐藏）') : s('None', '无')}`">
              <USelect v-model="draft.metadataAction" :items="metadataActionItems" />
            </UFormField>
            <UFormField v-if="!draft.secretsInUserScope && (!editing || draft.metadataAction === 'set')" :label="s('Authorization metadata URL (optional fallback)', '授权元数据 URL（可选回退）')">
              <UInput v-model="draft.metadataUrl" type="url" :required="editing && draft.metadataAction === 'set'" autocomplete="off" />
            </UFormField>
            <p>{{ s('Client secrets are entered under server details when signing in, never in saved configuration.', '客户端密钥在服务器详情中登录时输入，绝不写入配置。') }}</p>
          </section>
        </template>
        <section v-if="editing && !draft.secretsInUserScope && draft.type !== 'stdio'">
          <h4>{{ s('Bearer token (write-only)', 'Bearer 令牌（只写）') }}</h4>
          <p>{{ draft.authConfigured ? s('A token is configured.', '已配置令牌。') : s('No token is configured.', '未配置令牌。') }}</p>
          <UFormField :label="s('Token action', '令牌操作')">
            <USelect v-model="draft.bearerAction" :items="bearerActionItems" />
          </UFormField>
          <UFormField v-if="draft.bearerAction === 'set'" :label="s('New bearer token', '新 Bearer 令牌')">
            <UInput v-model="draft.token" type="password" autocomplete="new-password" required />
          </UFormField>
        </section>
        <section v-if="!draft.secretsInUserScope">
          <h4>{{ draft.type === 'stdio' ? s('Environment variables', '环境变量') : s('HTTP headers', 'HTTP 请求头') }}</h4>
          <p>{{ s('Stored values are never returned. Keep preserves a value; Set replaces it; Remove deletes it. Values are not stored in this browser.', '已存储的值不会返回。“保留”维持原值，“设置”替换原值，“删除”移除字段。值不会保存在此浏览器中。') }}</p>
          <div v-for="(secret, index) in draft.secrets" :key="index" class="secret-row">
            <UInput v-model="secret.name" :aria-label="s('Field name', '字段名称')" :readonly="secret.stored" required />
            <USelect v-model="secret.action" :items="secretActionItems(secret.stored)" :aria-label="s('Secret action', '机密操作')" />
            <UInput v-if="secret.action === 'set'" v-model="secret.value" type="password" autocomplete="new-password" :aria-label="s('New value', '新值')" />
            <UButton size="sm" color="neutral" variant="soft" @click="draft.secrets.splice(index, 1)">{{ s('Cancel field change', '取消字段更改') }}</UButton>
          </div>
          <UButton size="sm" color="neutral" variant="soft" @click="draft.secrets.push({ name: '', action: 'set', value: '' })">{{ s('Add field', '添加字段') }}</UButton>
        </section>
        <UCheckbox v-if="!editing && draft.source === 'user'" v-model="draft.enabled" :label="s('Enable after saving', '保存后启用')" />
        <p v-if="draft.source === 'project'">{{ s('Project tests require a saved entry that you explicitly enabled locally. Unsaved project definitions cannot execute.', '项目测试要求条目已保存且由您在本地明确启用。未保存的项目定义不能执行。') }}</p>
        <p v-if="editing">{{ s('Enablement and unedited fields are preserved. Use the server actions to enable, disable or sign in.', '启用状态及未编辑字段会被保留。请通过服务器操作启用、禁用或登录。') }}</p>
        <div class="settings-actions">
          <UButton color="neutral" variant="soft" :disabled="!connection.supports('mcp.test') || (draft.source === 'project' && (!editing || !draft.enabled))" @click="testServer()">{{ s('Test draft connection', '测试草稿连接') }}</UButton>
          <UButton type="submit" color="primary" :disabled="!dirty || (editing && !connection.supports('mcp.update'))">{{ s('Save server', '保存服务器') }}</UButton>
          <UButton color="neutral" variant="soft" @click="closeEditor">{{ s('Cancel', '取消') }}</UButton>
        </div>
      </fieldset>
    </form>
    <section v-if="testResult" class="settings-card" role="status">
      <h3 v-if="testResult.probe.status === 'connected'">{{ s('Connection test passed', '连接测试通过') }}: {{ testResult.id }} · {{ testResult.probe.latencyMs }} ms</h3>
      <h3 v-else-if="testResult.probe.status === 'authRequired'">{{ s('Sign-in required', '需要登录') }}: {{ testResult.id }} · {{ testResult.probe.latencyMs }} ms</h3>
      <h3 v-else>{{ s('Connection test failed', '连接测试失败') }}: {{ testResult.id }} · {{ testResult.probe.latencyMs }} ms</h3>
      <p v-if="testResult.probe.status === 'connected'">{{ s('The temporary connection has been closed. Configuration and session connections were not changed.', '临时连接已关闭。配置及会话连接未被更改。') }}</p>
      <template v-else-if="testResult.probe.status === 'authRequired'">
        <p>{{ testResult.probe.expired ? s('The stored credential was rejected. Sign in again.', '已存储的凭据被拒绝，请重新登录。') : s('This server requires sign-in.', '此服务器需要登录。') }}</p>
        <p v-if="testResult.probe.missingScopes?.length">{{ s('Additional scopes', '额外权限范围') }}: {{ testResult.probe.missingScopes.join(', ') }}</p>
      </template>
      <p v-else>{{ reasonText(testResult.probe.reason) }}<template v-if="testResult.probe.message"> · {{ testResult.probe.message }}</template></p>
      <ul v-if="testResult.probe.tools"><li v-for="tool in testResult.probe.tools" :key="tool.name"><strong>{{ tool.name }}</strong><p v-if="tool.description">{{ tool.description }}</p></li></ul>
      <p v-if="testResult.probe.status === 'connected' && !testResult.probe.tools?.length">{{ s('The server exposes no tools.', '服务器没有提供工具。') }}</p>
    </section>
    <section v-if="authorization" class="settings-card">
      <h3>{{ s('Authorize MCP', '授权 MCP') }}: {{ authorizingId }}</h3>
      <p>{{ s('Callback', '回调') }}: <code>{{ authorization.redirectUri }}</code> · {{ s('Expires', '过期时间') }}: {{ new Date(authorization.expiresAt).toLocaleString() }}</p>
      <UButton color="primary" :href="authorization.authorizationUrl" target="_blank" rel="noopener noreferrer">{{ s('Open authorization page', '打开授权页面') }}</UButton>
      <form class="settings-form" autocomplete="off" @submit.prevent="finishAuth()">
        <UFormField :label="s('Paste the full callback URL after authorization', '授权后粘贴完整回调 URL')">
          <UInput v-model="callback" type="password" autocomplete="new-password" :disabled="busy" required />
        </UFormField>
        <div class="settings-actions">
          <UButton type="submit" color="primary" :disabled="busy || !connection.supports('mcp.auth.complete')">{{ s('Complete sign-in', '完成登录') }}</UButton>
          <UButton color="neutral" variant="soft" :disabled="busy || !connection.supports('mcp.auth.cancel')" @click="finishAuth(true)">{{ s('Cancel sign-in', '取消登录') }}</UButton>
        </div>
      </form>
    </section>
    <UFormField class="search-label" :label="s('Search servers', '搜索服务器')"><UInput v-model="search" type="search" /></UFormField>
    <section v-for="server in entries" :key="`${server.source}:${server.id}`" class="settings-card">
      <h3>{{ server.id }}</h3>
      <p>{{ server.source }} · {{ server.transport }} · {{ server.enabled ? s('Enabled', '已启用') : s('Disabled', '已禁用') }}<template v-if="server.auth"> · OAuth: {{ server.auth }}</template></p>
      <div class="settings-actions">
        <UButton color="neutral" variant="soft" :disabled="busy || !connection.supports('mcp.inspect')" @click="inspect(server)">{{ s('Details / tools', '详情 / 工具') }}</UButton>
        <UButton color="neutral" variant="soft" :disabled="busy || !!draft || !connection.supports('mcp.test') || (server.source !== 'user' && !server.enabled)" @click="testServer(server)">{{ s('Test connection', '测试连接') }}</UButton>
        <template v-if="server.source !== 'plugin'">
          <UButton color="neutral" variant="soft" :disabled="busy || !!draft || !connection.supports('mcp.configuration') || !connection.supports('mcp.update')" @click="edit(server)">{{ s('Edit', '编辑') }}</UButton>
          <UButton color="neutral" variant="soft" :disabled="busy || !!draft || !connection.supports('mcp.move') || (server.source === 'user' && !loadedCwd)" @click="move(server)">{{ server.source === 'user' ? s('Move to project…', '移入项目…') : s('Move to user…', '移到用户…') }}</UButton>
          <UButton color="neutral" variant="soft" :disabled="busy || !!draft || !connection.supports('mcp.set_enabled')" @click="action('mcp.set_enabled', server)">{{ server.enabled ? s('Disable', '禁用') : s('Enable', '启用') }}</UButton>
          <UButton color="error" variant="ghost" :disabled="busy || !!draft || !connection.supports('mcp.remove')" @click="action('mcp.remove', server)">{{ s('Remove', '移除') }}</UButton>
        </template>
        <UButton color="neutral" variant="soft" :disabled="busy || !server.enabled || !connection.supports('mcp.disconnect')" @click="action('mcp.disconnect', server)">{{ s('Disconnect', '断开连接') }}</UButton>
        <template v-if="server.transport !== 'stdio'">
          <UButton color="neutral" variant="soft" :disabled="busy || !!authorization || !server.enabled || !connection.supports('mcp.auth.start')" @click="action('mcp.auth.start', server)">{{ s('Sign in', '登录') }}</UButton>
          <UButton v-if="server.auth" color="neutral" variant="soft" :disabled="busy || !connection.supports('mcp.auth.logout')" @click="action('mcp.auth.logout', server)">{{ s('Sign out', '退出登录') }}</UButton>
        </template>
      </div>
      <p v-if="server.source === 'plugin'">{{ s('This entry is owned by its plugin. Change it through Plugins.', '此条目归其插件所有。请通过插件页面更改。') }}</p>
    </section>
    <p v-if="catalog && !entries.length && !invalidEntries.length" role="status">{{ s('No matching MCP servers. Add a server to get started.', '没有匹配的 MCP 服务器。请添加服务器以开始使用。') }}</p>
  </article>
</template>

<style scoped>
fieldset { display: grid; gap: var(--space-4); min-width: 0; padding: 0; border: 0; }
.argument-row { display: flex; align-items: center; gap: var(--space-2); margin-block: var(--space-2); }
.argument-row > :first-child { flex: 1; min-width: 0; }
.secret-row { display: grid; grid-template-columns: 1fr auto 1fr auto; gap: var(--space-2); margin-block: var(--space-3); }
.search-label { margin-block: var(--space-4); }
.runtime-detail { border-top: 1px solid var(--border); padding-top: var(--space-3); }
code, li p { overflow-wrap: anywhere; }
@media (max-width: 700px) { .secret-row { grid-template-columns: 1fr 1fr; } }
</style>
