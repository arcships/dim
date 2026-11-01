<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue"
import { useSettingsText } from "../../i18n/settings-general"
import { readMcp, readMcpAuthStart, readMcpEditableEntry, readMcpInspection, readMcpTest, type McpConfigurationPatch, type McpEntry, type McpInspection, type McpOAuthInput, type McpProbe, type McpServerInput, type McpView } from "../../rpc/general-settings"
import { readAck } from "../../rpc/read"
import { useConnectionStore } from "../../stores/connection"
import GeneralSettingsStatus from "./GeneralSettingsStatus.vue"
import { bindSettingsDirty, messageFrom, reloadSettingsOnReconnect } from "./context"

type SecretField = { name: string; action: "keep" | "set" | "remove"; value: string; stored?: boolean }
type Draft = {
  id: string; source: "user" | "project"; type: "stdio" | "http" | "sse"
  command: string; args: string[]; cwd: string; url: string; endpoint?: string
  argsAction: "keep" | "replace"; storedArgsCount: number
  secrets: SecretField[]; enabled: boolean; token: string; authConfigured: boolean
  bearerAction: "keep" | "set" | "remove"; oauth: boolean; oauthAction: "keep" | "set" | "remove"
  clientId: string; callbackPort: string; scopes: string; metadataUrl: string
  metadataConfigured: boolean; metadataAction: "keep" | "set" | "remove"
}
const s = useSettingsText()
const connection = useConnectionStore()
const catalog = ref<McpView>()
const cwd = ref("")
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
const editorForm = ref<HTMLFormElement>()
const search = ref("")
let mounted = true
const dirty = computed(() => draft.value !== undefined && JSON.stringify(draft.value) !== baseline.value)
const oauthEnabled = computed(() => draft.value !== undefined && (editing.value ? draft.value.oauthAction === "set" || (draft.value.oauthAction === "keep" && draft.value.oauth) : draft.value.oauth))
const entries = computed(() => catalog.value?.servers.filter((server) => server.id.toLocaleLowerCase().includes(search.value.toLocaleLowerCase())) ?? [])
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
async function load(): Promise<void> {
  if (busy.value) return
  if (dirty.value && !window.confirm(s('Discard unsaved MCP configuration?', '放弃未保存的 MCP 配置？'))) return
  busy.value = true
  error.value = ""
  try {
    if (!connection.supports("mcp.catalog")) throw new Error(s('MCP management is not available on this server.', '此服务器不支持 MCP 管理。'))
    const path = cwd.value.trim()
    const next = await connection.client.call("mcp.catalog", path ? { cwd: path } : {}, readMcp)
    loadedCwd.value = path
    catalog.value = next
    draft.value = undefined
    detail.value = undefined
    clientSecret.value = ""
    testResult.value = undefined
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
function create(): void {
  if (dirty.value && !window.confirm(s('Discard unsaved MCP configuration?', '放弃未保存的 MCP 配置？'))) return
  draft.value = { id: "", source: "user", type: "stdio", command: "", args: [], argsAction: "replace", storedArgsCount: 0, cwd: "", url: "", secrets: [], enabled: false, oauth: false, clientId: "", token: "", bearerAction: "keep", authConfigured: false, oauthAction: "keep", callbackPort: "", scopes: "", metadataUrl: "", metadataConfigured: false, metadataAction: "keep" }
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
function configuration(form: Draft): McpServerInput {
  const values = Object.fromEntries(form.secrets.filter((secret) => secret.action === "set").map((secret) => [secret.name.trim(), secret.value]))
  const enabled = form.source === "user" && form.enabled
  if (form.type === "stdio") return { type: "stdio", command: form.command, args: form.args, ...(form.cwd ? { cwd: form.cwd } : {}), env: values, enabled }
  const oauth: McpOAuthInput = {
    ...(form.clientId.trim() ? { clientId: form.clientId.trim() } : {}),
    ...(form.callbackPort ? { callbackPort: Number(form.callbackPort) } : {}),
    ...(form.scopes.trim() ? { scopes: form.scopes.trim() } : {}),
    ...(form.metadataUrl.trim() ? { authServerMetadataUrl: form.metadataUrl.trim() } : {}),
  }
  return { type: form.type, url: form.url.trim(), headers: values, enabled, ...(form.token ? { auth: { type: "bearer", token: form.token } as const } : {}), ...(form.oauth ? { oauth } : {}) }
}
function patch(form: Draft): McpConfigurationPatch {
  const secrets = Object.fromEntries(form.secrets.filter((secret) => secret.action !== "keep").map((secret) => [secret.name.trim(), secret.action === "remove" ? null : secret.value]))
  if (form.type === "stdio") return { command: form.command, ...(form.argsAction === "replace" ? { args: form.args } : {}), cwd: form.cwd, env: secrets }
  const value: McpConfigurationPatch = { ...(form.url.trim() ? { url: form.url.trim() } : {}), headers: secrets }
  if (form.bearerAction !== "keep") value.bearer = form.bearerAction === "remove" ? { action: "remove" } : { action: "set", token: form.token }
  if (form.oauthAction === "remove") value.oauth = { action: "remove" }
  if (form.oauthAction === "set") value.oauth = {
    action: "set", clientId: form.clientId.trim() || null, callbackPort: form.callbackPort ? Number(form.callbackPort) : null, scopes: form.scopes.trim() || null,
    ...(form.metadataAction === "remove" ? { authServerMetadataUrl: null } : form.metadataAction === "set" ? { authServerMetadataUrl: form.metadataUrl.trim() } : {}),
  }
  return value
}
function validDraft(form: Draft): boolean {
  const names = form.secrets.map((secret) => secret.name.trim())
  if (names.some((name) => !name) || new Set(names).size !== names.length) {
    error.value = s('Secret field names must be nonempty and unique.', '机密字段名称不能为空且必须唯一。')
    return false
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
reloadSettingsOnReconnect(connection, load, computed(() => draft.value !== undefined || !!clientSecret.value || !!callback.value || !!authorization.value))
onMounted(load)
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
    <form class="settings-card settings-form" @submit.prevent="load"><label><span>{{ s('Project directory (optional, absolute path)', '项目目录（可选，绝对路径）') }}</span><input v-model="cwd" class="ui-field" data-size="md" :disabled="busy" /></label><div class="settings-actions"><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy">{{ s('Load / refresh', '加载 / 刷新') }}</button><button class="ui-button" data-size="md" data-variant="primary" type="button" :disabled="busy || !catalog || !connection.supports('mcp.save')" @click="create">{{ s('Add server', '添加服务器') }}</button></div></form>
    <GeneralSettingsStatus :busy="busy" :error="error" @reload="load" />
    <p v-for="issue in catalog?.errors" :key="issue" class="ui-banner settings-banner" data-tone="warning">{{ issue }}</p>
    <section v-if="detail" class="settings-card settings-form" aria-live="polite">
      <h3>{{ s('Server details', '服务器详情') }}: {{ detail.server.id }}</h3>
      <p>{{ detail.server.source }} · {{ detail.server.transport }} · {{ detail.inspection.enabled ? s('Enabled', '已启用') : s('Disabled', '已禁用') }}</p>
      <template v-if="detail.inspection.configuration.type === 'stdio'"><p>{{ s('Executable', '可执行程序') }}: <code>{{ detail.inspection.configuration.command }}</code></p><p>{{ s('Stored arguments (hidden)', '已存参数（隐藏）') }}: {{ detail.inspection.configuration.argsCount }}</p><p>{{ s('Working directory', '工作目录') }}: {{ detail.inspection.configuration.cwd || s('Daemon directory', '守护进程目录') }}</p><p>{{ s('Environment names', '环境变量名') }}: {{ detail.inspection.configuration.envNames.join(', ') || '—' }}</p></template>
      <template v-else><p>{{ s('Endpoint (origin only)', '端点（仅来源）') }}: <code>{{ detail.inspection.configuration.endpoint }}</code></p><p>{{ s('Header names', '请求头名称') }}: {{ detail.inspection.configuration.headerNames.join(', ') || '—' }}</p><p>{{ s('Stored bearer token', '已存储的 Bearer 令牌') }}: {{ detail.inspection.configuration.authConfigured ? s('Configured', '已配置') : s('None', '无') }}</p><p v-if="detail.inspection.configuration.oauth?.clientId">OAuth client ID: {{ detail.inspection.configuration.oauth.clientId }}</p></template>
      <p>{{ s('Connection snapshots do not open a connection. Tool lists are the last successful discovery for each live connection generation.', '查看快照不会打开连接。工具列表来自每个活动连接代次最近一次成功的发现。') }}</p>
      <p v-if="!detail.inspection.connections.length" role="status">{{ s('No live session connections. Use Test connection to discover tools without creating a session.', '没有活动会话连接。可通过“测试连接”发现工具，无需创建会话。') }}</p>
      <section v-for="(runtime, index) in detail.inspection.connections" :key="index" class="runtime-detail"><h4>{{ s('Connection', '连接') }} {{ index + 1 }} · {{ runtime.state }}<template v-if="runtime.expired"> · {{ s('Credential expired', '凭据已过期') }}</template></h4><ul v-if="runtime.tools"><li v-for="tool in runtime.tools" :key="tool.name"><strong>{{ tool.name }}</strong><p v-if="tool.description">{{ tool.description }}</p></li></ul><p v-if="runtime.tools === undefined">{{ s('Tools have not been discovered.', '尚未发现工具。') }}</p><p v-else-if="!runtime.tools.length">{{ s('The server exposes no tools.', '服务器没有提供工具。') }}</p></section>
      <label v-if="detail.inspection.configuration.type !== 'stdio' && detail.inspection.configuration.oauth?.clientId"><span>{{ s('OAuth client secret (optional, sent once on Sign in)', 'OAuth 客户端密钥（可选，仅在登录时发送一次）') }}</span><input v-model="clientSecret" class="ui-field" data-size="md" type="password" autocomplete="new-password" :disabled="busy" /></label>
      <div class="settings-actions"><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy" @click="inspect(detail.server)">{{ s('Refresh details', '刷新详情') }}</button><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy || !connection.supports('mcp.test') || (detail.server.source !== 'user' && !detail.server.enabled)" @click="testServer(detail.server)">{{ s('Test connection', '测试连接') }}</button><button v-if="detail.server.transport !== 'stdio'" class="ui-button" data-size="md" data-variant="secondary" :disabled="busy || !!authorization || !detail.server.enabled || !connection.supports('mcp.auth.start')" @click="action('mcp.auth.start', detail.server)">{{ s('Sign in', '登录') }}</button></div>
    </section>
    <form v-if="draft" ref="editorForm" class="settings-card settings-form" autocomplete="off" @submit.prevent="save"><h3>{{ editing ? s('Edit server', '编辑服务器') : s('New server', '新服务器') }}</h3><fieldset :disabled="busy"><label><span>{{ s('Server ID', '服务器 ID') }}</span><input v-model="draft.id" class="ui-field" data-size="md" :disabled="editing" required /></label><label><span>{{ s('Source', '来源') }}</span><select v-model="draft.source" class="ui-field" data-size="md" :disabled="editing"><option value="user">{{ s('User', '用户') }}</option><option v-if="loadedCwd" value="project">{{ s('Project', '项目') }}</option></select></label><p v-if="editing">{{ s('The source and transport are fixed. Changes write only this source; moving between user and project scopes is not an atomic owner operation.', '来源和传输方式固定。更改只写入此来源；在用户和项目范围之间迁移不是所有者支持的原子操作。') }}</p><label><span>{{ s('Transport', '传输方式') }}</span><select v-model="draft.type" class="ui-field" data-size="md" :disabled="editing"><option value="stdio">stdio</option><option value="http">HTTP</option><option value="sse">SSE</option></select></label>
      <template v-if="draft.type === 'stdio'">
        <label><span>{{ s('Executable', '可执行程序') }}</span><input v-model="draft.command" class="ui-field" data-size="md" required /></label>
        <label><span>{{ s('Working directory (optional)', '工作目录（可选）') }}</span><input v-model="draft.cwd" class="ui-field" data-size="md" /></label>
        <label v-if="editing"><span>{{ s('Stored arguments (hidden)', '已存参数（隐藏）') }}: {{ draft.storedArgsCount }}</span><select v-model="draft.argsAction" class="ui-field"><option value="keep">{{ s('Keep stored arguments', '保留已存参数') }}</option><option value="replace">{{ s('Replace all arguments', '替换全部参数') }}</option></select></label>
        <div v-if="!editing || draft.argsAction === 'replace'"><strong>{{ s('Arguments (one per field)', '参数（每个字段一个）') }}</strong><div v-for="(_, index) in draft.args" :key="index" class="argument-row"><input v-model="draft.args[index]" class="ui-field" data-size="md" type="password" autocomplete="new-password" :aria-label="s('Argument', '参数')" /><button class="ui-button" data-size="md" data-variant="secondary" type="button" @click="draft.args.splice(index, 1)">{{ s('Remove', '移除') }}</button></div><button class="ui-button" data-size="sm" data-variant="secondary" type="button" @click="draft.args.push('')">{{ s('Add argument', '添加参数') }}</button></div>
      </template>
      <template v-else>
        <p v-if="editing">{{ s('Current endpoint', '当前端点') }}: {{ draft.endpoint }} — {{ s('Path and credentials are hidden.', '路径和凭据已隐藏。') }}</p>
        <label><span>{{ editing ? s('Replace URL (blank preserves the stored URL)', '替换 URL（留空保留已存储的 URL）') : 'URL' }}</span><input v-model="draft.url" class="ui-field" data-size="md" type="url" :required="!editing" autocomplete="off" /></label>
        <template v-if="!editing"><label class="checkbox-label"><input v-model="draft.oauth" type="checkbox" :disabled="!!draft.token" />{{ s('Configure OAuth', '配置 OAuth') }}</label><label v-if="!draft.oauth"><span>{{ s('Bearer token (optional, write-only)', 'Bearer 令牌（可选，只写）') }}</span><input v-model="draft.token" class="ui-field" data-size="md" type="password" autocomplete="new-password" /></label></template>
        <label v-if="editing"><span>{{ s('OAuth configuration', 'OAuth 配置') }}</span><select v-model="draft.oauthAction" class="ui-field" data-size="md"><option value="keep">{{ s('Preserve existing configuration', '保留现有配置') }}</option><option value="set">{{ s('Configure / edit OAuth', '配置 / 编辑 OAuth') }}</option><option value="remove">{{ s('Remove explicit configuration (automatic discovery remains)', '移除显式配置（仍可自动发现）') }}</option></select></label>
        <section v-if="(!editing && draft.oauth) || (editing && draft.oauthAction === 'set')" class="settings-form">
          <p>{{ s('OAuth cannot be combined with a bearer token or Authorization header. Remove those explicitly when enabling OAuth. Leave optional fields blank to use defaults.', 'OAuth 不能与 Bearer 令牌或 Authorization 请求头一起使用。启用 OAuth 时请明确移除这些字段。可选字段留空使用默认值。') }}</p>
          <label><span>{{ s('OAuth client ID (optional)', 'OAuth 客户端 ID（可选）') }}</span><input v-model="draft.clientId" class="ui-field" data-size="md" /></label>
          <label><span>{{ s('Callback port (default 1456)', '回调端口（默认 1456）') }}</span><input v-model="draft.callbackPort" class="ui-field" data-size="md" type="number" min="1" max="65535" step="1" placeholder="1456" /></label>
          <label><span>{{ s('Scopes (space separated, optional)', '权限范围（空格分隔，可选）') }}</span><input v-model="draft.scopes" class="ui-field" data-size="md" /></label>
          <label v-if="editing"><span>{{ s('Authorization metadata URL', '授权元数据 URL') }} · {{ draft.metadataConfigured ? s('Configured (hidden)', '已配置（隐藏）') : s('None', '无') }}</span><select v-model="draft.metadataAction" class="ui-field" data-size="md"><option value="keep">{{ s('Keep', '保留') }}</option><option value="set">{{ s('Replace', '替换') }}</option><option value="remove">{{ s('Remove', '移除') }}</option></select></label>
          <label v-if="!editing || draft.metadataAction === 'set'"><span>{{ s('Authorization metadata URL (optional fallback)', '授权元数据 URL（可选回退）') }}</span><input v-model="draft.metadataUrl" class="ui-field" data-size="md" type="url" :required="editing && draft.metadataAction === 'set'" autocomplete="off" /></label>
          <p>{{ s('Client secrets are entered under server details when signing in, never in saved configuration.', '客户端密钥在服务器详情中登录时输入，绝不写入配置。') }}</p>
        </section>
      </template>
      <section v-if="editing && draft.type !== 'stdio'"><h4>{{ s('Bearer token (write-only)', 'Bearer 令牌（只写）') }}</h4><p>{{ draft.authConfigured ? s('A token is configured.', '已配置令牌。') : s('No token is configured.', '未配置令牌。') }}</p><label><span>{{ s('Token action', '令牌操作') }}</span><select v-model="draft.bearerAction" class="ui-field" data-size="md"><option value="keep">{{ s('Keep stored token', '保留已存储的令牌') }}</option><option value="set" :disabled="oauthEnabled">{{ s('Replace token', '替换令牌') }}</option><option value="remove">{{ s('Remove token', '移除令牌') }}</option></select></label><label v-if="draft.bearerAction === 'set'"><span>{{ s('New bearer token', '新 Bearer 令牌') }}</span><input v-model="draft.token" class="ui-field" data-size="md" type="password" autocomplete="new-password" required /></label></section>
      <section><h4>{{ draft.type === 'stdio' ? s('Environment variables', '环境变量') : s('HTTP headers', 'HTTP 请求头') }}</h4><p>{{ s('Stored values are never returned. Keep preserves a value; Set replaces it; Remove deletes it. Values are not stored in this browser.', '已存储的值不会返回。“保留”维持原值，“设置”替换原值，“删除”移除字段。值不会保存在此浏览器中。') }}</p><div v-for="(secret, index) in draft.secrets" :key="index" class="secret-row"><input v-model="secret.name" class="ui-field" data-size="md" :aria-label="s('Field name', '字段名称')" :readonly="secret.stored" required /><select v-model="secret.action" class="ui-field" data-size="md" :aria-label="s('Secret action', '机密操作')"><option v-if="secret.stored" value="keep">{{ s('Keep', '保留') }}</option><option value="set">{{ s('Set', '设置') }}</option><option v-if="secret.stored" value="remove">{{ s('Remove', '删除') }}</option></select><input v-if="secret.action === 'set'" v-model="secret.value" class="ui-field" data-size="md" type="password" autocomplete="new-password" :aria-label="s('New value', '新值')" /><button class="ui-button" data-size="sm" data-variant="secondary" type="button" @click="draft.secrets.splice(index, 1)">{{ s('Cancel field change', '取消字段更改') }}</button></div><button class="ui-button" data-size="sm" data-variant="secondary" type="button" @click="draft.secrets.push({ name: '', action: 'set', value: '' })">{{ s('Add field', '添加字段') }}</button></section>
      <label v-if="!editing && draft.source === 'user'" class="checkbox-label"><input v-model="draft.enabled" type="checkbox" />{{ s('Enable after saving', '保存后启用') }}</label><p v-if="draft.source === 'project'">{{ s('Project tests require a saved entry that you explicitly enabled locally. Unsaved project definitions cannot execute.', '项目测试要求条目已保存且由您在本地明确启用。未保存的项目定义不能执行。') }}</p><p v-if="editing">{{ s('Enablement and unedited fields are preserved. Use the server actions to enable, disable or sign in.', '启用状态及未编辑字段会被保留。请通过服务器操作启用、禁用或登录。') }}</p><div class="settings-actions"><button class="ui-button" data-size="md" data-variant="secondary" type="button" :disabled="!connection.supports('mcp.test') || (draft.source === 'project' && (!editing || !draft.enabled))" @click="testServer()">{{ s('Test draft connection', '测试草稿连接') }}</button><button class="ui-button" data-size="md" data-variant="primary" :disabled="!dirty || (editing && !connection.supports('mcp.update'))">{{ s('Save server', '保存服务器') }}</button><button class="ui-button" data-size="md" data-variant="secondary" type="button" @click="closeEditor">{{ s('Cancel', '取消') }}</button></div></fieldset></form>
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
    <section v-if="authorization" class="settings-card"><h3>{{ s('Authorize MCP', '授权 MCP') }}: {{ authorizingId }}</h3><p>{{ s('Callback', '回调') }}: <code>{{ authorization.redirectUri }}</code> · {{ s('Expires', '过期时间') }}: {{ new Date(authorization.expiresAt).toLocaleString() }}</p><a class="ui-button" data-size="md" data-variant="primary" :href="authorization.authorizationUrl" target="_blank" rel="noopener noreferrer">{{ s('Open authorization page', '打开授权页面') }}</a><form class="settings-form" autocomplete="off" @submit.prevent="finishAuth()"><label><span>{{ s('Paste the full callback URL after authorization', '授权后粘贴完整回调 URL') }}</span><input v-model="callback" class="ui-field" data-size="md" type="password" autocomplete="new-password" :disabled="busy" required /></label><div class="settings-actions"><button class="ui-button" data-size="md" data-variant="primary" :disabled="busy || !connection.supports('mcp.auth.complete')">{{ s('Complete sign-in', '完成登录') }}</button><button class="ui-button" data-size="md" data-variant="secondary" type="button" :disabled="busy || !connection.supports('mcp.auth.cancel')" @click="finishAuth(true)">{{ s('Cancel sign-in', '取消登录') }}</button></div></form></section>
    <label class="search-label"><span>{{ s('Search servers', '搜索服务器') }}</span><input v-model="search" class="ui-field" data-size="md" type="search" /></label>
    <section v-for="server in entries" :key="`${server.source}:${server.id}`" class="settings-card"><h3>{{ server.id }}</h3><p>{{ server.source }} · {{ server.transport }} · {{ server.enabled ? s('Enabled', '已启用') : s('Disabled', '已禁用') }}<template v-if="server.auth"> · OAuth: {{ server.auth }}</template></p><div class="settings-actions"><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy || !connection.supports('mcp.inspect')" @click="inspect(server)">{{ s('Details / tools', '详情 / 工具') }}</button><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy || !!draft || !connection.supports('mcp.test') || (server.source !== 'user' && !server.enabled)" @click="testServer(server)">{{ s('Test connection', '测试连接') }}</button><template v-if="server.source !== 'plugin'"><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy || !!draft || !connection.supports('mcp.configuration') || !connection.supports('mcp.update')" @click="edit(server)">{{ s('Edit', '编辑') }}</button><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy || !!draft || !connection.supports('mcp.set_enabled')" @click="action('mcp.set_enabled', server)">{{ server.enabled ? s('Disable', '禁用') : s('Enable', '启用') }}</button><button class="ui-button" data-size="md" data-variant="danger-ghost" :disabled="busy || !!draft || !connection.supports('mcp.remove')" @click="action('mcp.remove', server)">{{ s('Remove', '移除') }}</button></template><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy || !server.enabled || !connection.supports('mcp.disconnect')" @click="action('mcp.disconnect', server)">{{ s('Disconnect', '断开连接') }}</button><template v-if="server.transport !== 'stdio'"><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy || !!authorization || !server.enabled || !connection.supports('mcp.auth.start')" @click="action('mcp.auth.start', server)">{{ s('Sign in', '登录') }}</button><button v-if="server.auth" class="ui-button" data-size="md" data-variant="secondary" :disabled="busy || !connection.supports('mcp.auth.logout')" @click="action('mcp.auth.logout', server)">{{ s('Sign out', '退出登录') }}</button></template></div><p v-if="server.source === 'plugin'">{{ s('This entry is owned by its plugin. Change it through Plugins.', '此条目归其插件所有。请通过插件页面更改。') }}</p></section>
    <p v-if="catalog && !entries.length" role="status">{{ s('No matching MCP servers. Add a server to get started.', '没有匹配的 MCP 服务器。请添加服务器以开始使用。') }}</p>
  </article>
</template>

<style scoped>
fieldset { display: grid; gap: var(--space-4); min-width: 0; padding: 0; border: 0; }
label { display: grid; gap: var(--space-2); }
.checkbox-label, .argument-row { display: flex; align-items: center; gap: var(--space-2); }
.argument-row { margin-block: var(--space-2); }
.argument-row input { flex: 1; min-width: 0; }
.secret-row { display: grid; grid-template-columns: 1fr auto 1fr auto; gap: var(--space-2); margin-block: var(--space-3); }
.search-label { margin-block: var(--space-4); }
.runtime-detail { border-top: 1px solid var(--border); padding-top: var(--space-3); }
code, li p { overflow-wrap: anywhere; }
@media (max-width: 700px) { .secret-row { grid-template-columns: 1fr 1fr; } }
</style>
