import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { registerHooks, stripTypeScriptTypes } from "node:module"
import test from "node:test"
import { compileScript, parse } from "vue/compiler-sfc"
import { createRenderer, nextTick, reactive } from "vue"
import { nuxtUiTestPlugin } from "./helpers/nuxt-ui.mjs"

registerHooks({
  resolve(specifier, context, next) {
    if (context.parentURL?.endsWith("McpSettings.vue")) {
      if (specifier === "../../stores/connection") return { url: "data:text/javascript,export const useConnectionStore=()=>globalThis.settingsConnection", shortCircuit: true }
      if (specifier === "../../stores/sessions") return { url: "data:text/javascript,export const useSessionsStore=()=>({workspaces:[{cwd:'/project',name:'Project'}]})", shortCircuit: true }
    }
    if (specifier === "vue-i18n") return { url: "data:text/javascript,export const useI18n=()=>({locale:{value:'en'}})", shortCircuit: true }
    if (specifier.endsWith(".vue") && !specifier.endsWith("McpSettings.vue")) return { url: "data:text/javascript,export default { props:['busy','error','saved'], emits:['reload'], setup(props){return ()=>props.error||null} }", shortCircuit: true }
    try { return next(specifier, context) } catch (error) {
      if ((specifier.startsWith("./") || specifier.startsWith("../")) && !/\.(ts|vue)$/.test(specifier)) return next(`${specifier}.ts`, context)
      throw error
    }
  },
  load(url, context, next) {
    if (url.endsWith(".ts")) return { format: "module", source: stripTypeScriptTypes(readFileSync(new URL(url), "utf8"), { mode: "transform" }), shortCircuit: true }
    if (!url.endsWith("McpSettings.vue")) return next(url, context)
    const { descriptor } = parse(readFileSync(new URL(url), "utf8"))
    return { format: "module", source: stripTypeScriptTypes(compileScript(descriptor, { id: "settings-mcp-test", inlineTemplate: true }).content, { mode: "transform" }), shortCircuit: true }
  },
})

globalThis.Document = class {}
globalThis.ShadowRoot = class {}

const { readMcp, readMcpConfiguration } = await import("../src/rpc/general-settings.ts")
const { RpcError } = await import("../src/rpc/client.ts")
const { default: McpSettings } = await import("../src/features/settings/McpSettings.vue")

const node = (type, text = "") => ({
  type, text, children: [], props: {}, parent: undefined, value: "", options: [], selectedIndex: -1,
  addEventListener() {}, reportValidity: () => true, getRootNode() { return {} },
})
const renderer = createRenderer({
  createElement: type => node(type), createText: (text) => node("text", text), createComment: (text) => node("comment", text),
  patchProp: (element, key, _, value) => { element.props[key] = value },
  setText: (element, text) => { element.text = text },
  setElementText: (element, text) => { element.text = text; element.children = [] },
  parentNode: (element) => element.parent,
  nextSibling: (element) => element.parent?.children[element.parent.children.indexOf(element) + 1],
  insert(element, parent, anchor) {
    if (element.parent) element.parent.children.splice(element.parent.children.indexOf(element), 1)
    parent.children.splice(anchor ? parent.children.indexOf(anchor) : parent.children.length, 0, element)
    element.parent = parent
  },
  remove(element) {
    if (element.parent) element.parent.children.splice(element.parent.children.indexOf(element), 1)
    element.parent = undefined
  },
})
const text = (element) => (element.type === "comment" ? "" : element.text) + element.children.map(text).join("")
const all = (element) => [element, ...element.children.flatMap(all)]
const flush = async () => { for (let i = 0; i < 12; i++) { await Promise.resolve(); await nextTick() } }
const button = (root, label) => all(root).find((element) => element.type === "button" && text(element) === label)
const field = (root, label) => all(root).find((element) => element.type === "label" && text(element).startsWith(label)).children.find((element) => element.type === "input" || element.type === "select")
const enter = (element, value) => {
  if (element.props["onUpdate:modelValue"]) element.props["onUpdate:modelValue"](value)
  else element.props.onChange({ target: { value } })
}
const submit = (root) => all(root).find((element) => element.type === "form" && text(element).includes("Save server")).props.onSubmit({ preventDefault() {} })

function catalog(invalid) {
  return {
    userRevision: "user:1", projectRevision: null,
    userApplication: { error: null, cleanupError: null }, pluginApplication: null, projectApplication: null,
    servers: [{ id: "docs", source: "user", enabled: true, transport: "stdio" }],
    ...(invalid === undefined ? {} : { invalid }),
  }
}

test("a broken entry is read apart from the servers", () => {
  const error = "MCP server broken: field type must be http or sse; ws is not a supported transport"
  const view = readMcp(catalog([{ id: "broken", source: "project", error }]))
  assert.deepEqual(view.servers.map((server) => server.id), ["docs"])
  assert.deepEqual(view.invalid, [{ id: "broken", source: "project", error }])
})

test("a reply without the invalid list still reads its servers", () => {
  const view = readMcp(catalog(undefined))
  assert.deepEqual(view.servers.map((server) => server.id), ["docs"])
  assert.deepEqual(view.invalid, [])
})

test("settings show referenced and unset variable names", () => {
  const view = readMcp({
    ...catalog(undefined),
    servers: [{ id: "docs", source: "user", enabled: true, transport: "http",
      referencedVariables: ["MCP_HOST", "MCP_TOKEN"], unsetVariables: ["MCP_TOKEN"] }],
  })
  assert.deepEqual(view.servers[0].referencedVariables, ["MCP_HOST", "MCP_TOKEN"])
  assert.deepEqual(view.servers[0].unsetVariables, ["MCP_TOKEN"])
  const legacy = readMcp(catalog(undefined))
  assert.deepEqual(legacy.servers[0].referencedVariables, [])
  assert.deepEqual(legacy.servers[0].unsetVariables, [])
})

test("a configuration view names variables without values", () => {
  const configuration = readMcpConfiguration({
    type: "http", endpoint: "https://${MCP_HOST}", headerNames: ["Authorization"],
    authConfigured: true, oauthConfigured: false, oauth: null,
    referencedVariables: ["MCP_HOST", "MCP_TOKEN"], unsetVariables: ["MCP_TOKEN"],
  })
  assert.deepEqual(configuration.referencedVariables, ["MCP_HOST", "MCP_TOKEN"])
  assert.deepEqual(configuration.unsetVariables, ["MCP_TOKEN"])
  const legacy = readMcpConfiguration({ type: "stdio", command: "server", argsCount: 0, cwd: null, envNames: [] })
  assert.deepEqual(legacy.referencedVariables, [])
  assert.deepEqual(legacy.unsetVariables, [])
})

const { configuration, patch } = await import("../src/features/settings/mcp-form.ts")

function draft(overrides) {
  return {
    id: "docs", source: "user", type: "stdio", command: "server", args: [], cwd: "",
    argsAction: "replace", storedArgsCount: 0, secrets: [], enabled: false, token: "",
    authConfigured: false, bearerAction: "keep", oauth: false, oauthAction: "keep",
    clientId: "", callbackPort: "", scopes: "", metadataUrl: "", timeout: "",
    metadataConfigured: false, metadataAction: "keep", secretsInUserScope: false,
    ...overrides,
  }
}

test("a number-typed timeout saves and tests as the number", () => {
  const typed = draft({ timeout: 5000 })
  assert.equal(configuration(typed).timeout, 5000)
  assert.equal(patch(typed).timeout, 5000)
  const remote = draft({ type: "http", url: "https://example.test/mcp", timeout: 5000 })
  assert.equal(configuration(remote).timeout, 5000)
  assert.equal(patch(remote).timeout, 5000)
})

test("a kept string timeout still saves as the number", () => {
  const typed = draft({ timeout: "5000" })
  assert.equal(configuration(typed).timeout, 5000)
  assert.equal(patch(typed).timeout, 5000)
  const blank = draft({ timeout: "" })
  assert.equal("timeout" in configuration(blank), false)
  assert.equal(patch(blank).timeout, null)
})

/// #952: the daemon expands a reference, so the form accepts an endpoint
/// template and a malformed literal endpoint still stops the save.
test("an endpoint template is a valid draft and a malformed literal URL is not", async () => {
  const saves = []
  globalThis.window = { confirm: () => true }
  globalThis.settingsConnection = reactive({
    phase: "connected",
    supports: () => true,
    client: { call: async (method, params, read) => {
      if (method === "mcp.catalog") return read(catalog(undefined))
      assert.equal(method, "mcp.save")
      saves.push(JSON.parse(JSON.stringify(params)))
      return read({})
    } },
  })
  const root = node("root")
  const app = renderer.createApp(McpSettings)
  app.use(nuxtUiTestPlugin)
  app.mount(root)
  try {
    await flush()
    await button(root, "Add server").props.onClick()
    await flush()
    enter(field(root, "Server ID"), "template")
    enter(field(root, "Transport"), "http")
    await flush()
    const url = field(root, "URL")
    assert.equal(url.props.type, "text")
    assert.equal(url.props.inputmode, "url")
    enter(url, "${MCP_URL}/mcp")
    await flush()
    await submit(root)
    await flush()
    assert.equal(saves.length, 1)
    assert.equal(saves[0].configuration.type, "http")
    assert.equal(saves[0].configuration.url, "${MCP_URL}/mcp")

    await button(root, "Add server").props.onClick()
    await flush()
    enter(field(root, "Server ID"), "broken")
    enter(field(root, "Transport"), "http")
    await flush()
    enter(field(root, "URL"), "not a url")
    await flush()
    await submit(root)
    await flush()
    assert.equal(saves.length, 1, "the malformed literal URL never reaches the daemon")
    assert.match(text(root), /The endpoint must be an absolute URL/)
  } finally {
    app.unmount()
    delete globalThis.settingsConnection
    delete globalThis.window
  }
})

test("changing MCP scope keeps cancelled or failed drafts and saves only to the loaded workspace", async t => {
  let fail = false
  const saves = []
  globalThis.window = { confirm: () => true }
  globalThis.settingsConnection = reactive({
    phase: "connected", supports: () => true,
    client: { call: async (method, params, read) => {
      if (method === "mcp.catalog") {
        if (fail) throw new Error("project unavailable")
        return read({ ...catalog(undefined), projectRevision: params.cwd ? "project:1" : null })
      }
      assert.equal(method, "mcp.save")
      saves.push(params)
      return read({})
    } },
  })
  const root = node("root"), app = renderer.createApp(McpSettings)
  app.use(nuxtUiTestPlugin)
  app.mount(root)
  t.after(() => { app.unmount(); delete globalThis.settingsConnection; delete globalThis.window })
  await flush()
  await button(root, "Add server").props.onClick(); await flush()
  enter(field(root, "Server ID"), "new-server")
  enter(field(root, "Executable"), "server"); await flush()
  const scope = field(root, "Configuration scope")
  globalThis.window.confirm = () => false
  await scope.props["onUpdate:modelValue"]("/project"); await flush()
  assert.equal(scope.props.value, "__user__")
  assert.ok(button(root, "Save server"))
  globalThis.window.confirm = () => true
  fail = true
  await scope.props["onUpdate:modelValue"]("/project"); await flush()
  assert.equal(scope.props.value, "__user__")
  assert.match(text(root), /project unavailable/)
  await submit(root); await flush()
  assert.equal(saves[0].cwd, undefined)
  fail = false
  await scope.props["onUpdate:modelValue"]("/project"); await flush()
  await button(root, "Add server").props.onClick(); await flush()
  enter(field(root, "Server ID"), "project-server")
  enter(field(root, "Executable"), "server")
  enter(field(root, "Source"), "project"); await flush()
  await submit(root); await flush()
  assert.equal(saves[1].cwd, "/project")
  assert.equal(saves[1].source, "project")
  assert.equal(saves[1].expectedVersion, "project:1")
})

/// The owner's import preview for `entries`, read at `revision`.
function importPreviewReply(entries, revision = "user:1", unknownTopLevel = [], sourceRevision, store) {
  return { revision, entries, unknownTopLevel, ...(sourceRevision === undefined ? {} : { sourceRevision }), ...(store === undefined ? {} : { store }) }
}
const importEntry = (overrides) => ({ id: "docs", transport: "stdio", status: "changed", changes: [], unsupported: [], omitted: 0, ...overrides })
function mountImport({ preview, catalogs, apply, sources = [] }) {
  const calls = []
  let served = 0
  globalThis.window = { confirm: () => true }
  globalThis.settingsConnection = reactive({
    phase: "connected", supports: () => true,
    client: { call: async (method, params, read) => {
      calls.push({ method, params })
      if (method === "mcp.catalog") {
        const reply = catalogs[Math.min(served, catalogs.length - 1)]
        served += 1
        return read(reply)
      }
      if (method === "mcp.import.sources") return read({ sources })
      if (method === "mcp.import.preview") return read(preview)
      if (method === "mcp.import.apply") {
        if (apply) return apply(params, read)
        return read({ revision: "user:2" })
      }
      throw new Error(`unexpected method ${method}`)
    } },
  })
  const root = node("root")
  const app = renderer.createApp(McpSettings)
  app.use(nuxtUiTestPlugin)
  app.mount(root)
  const mounted = flush().then(() => ({ root, app, calls }))
  return mounted
}
async function chooseImportFile(root, document) {
  const input = all(root).find((element) => element.type === "input" && element.props.type === "file")
  await input.props.onChange({ target: { files: [{ text: async () => document }], value: "" } })
  await flush()
}
const importSelects = (root) => all(root).filter((element) => element.type === "select" && element.props["aria-label"] === "Import action")
const closeImport = (app) => { app.unmount(); delete globalThis.settingsConnection; delete globalThis.window }

test("an import preview shows every status and its fields without a value", async () => {
  const secret = "sk-web-secret-never-print"
  const document = `{"mcpServers":{"docs":{"env":{"TOKEN":"${secret}"}}}}`
  const preview = importPreviewReply([
    importEntry({ id: "keeps", status: "identical" }),
    importEntry({ id: "docs", changes: [{ field: "args", kind: "array" }, { field: "enabled", kind: "boolean" }], unsupported: [{ field: "x-extra", effect: "kept-unused" }], omitted: 7 }),
    importEntry({ id: "taken", transport: "http", status: "conflict", changes: [{ field: "url", kind: "string" }] }),
    importEntry({ id: "broken", transport: undefined, status: "invalid", unsupported: [{ field: "auth.audience", effect: "dropped" }], error: "broken: type is stdio, http or sse" }),
  ], "user:1", ["otherTopLevel"])
  const { root, app, calls } = await mountImport({ preview, catalogs: [catalog(undefined)] })
  try {
    await chooseImportFile(root, document)
    const previewCall = calls.find((call) => call.method === "mcp.import.preview")
    assert.equal(previewCall.params.source, "user")
    assert.equal(previewCall.params.document, document)
    assert.equal(previewCall.params.document.includes(secret), true)
    assert.equal("cwd" in previewCall.params, false)
    const shown = text(root)
    assert.match(shown, /keeps · identical · stdio/)
    assert.match(shown, /docs · changed · stdio/)
    assert.match(shown, /Changed fields: args \(array\), enabled \(boolean\)/)
    assert.match(shown, /Not carried: x-extra \(kept unused\)/)
    assert.match(shown, /7 more field names are not listed\./)
    assert.match(shown, /taken · conflict · http/)
    assert.match(shown, /broken · invalid/)
    assert.match(shown, /auth\.audience \(dropped\)/)
    assert.match(shown, /broken: type is stdio, http or sse/)
    assert.match(shown, /Top-level members the import does not carry: otherTopLevel\./)
    assert.equal(shown.includes(secret), false)
    assert.match(shown, /Import into: user configuration/)
    // A preview of the page's own document names no store.
    assert.equal(shown.includes("From store"), false)
    // Every entry that decodes has an action; the invalid one can only skip.
    assert.equal(importSelects(root).length, 3)
    assert.match(shown, /This entry does not decode, so it can only be skipped\./)
    // No default action is a rename, so no entry asks for a new id.
    assert.equal(all(root).find((element) => element.type === "label" && text(element).startsWith("New id")), undefined)
    assert.equal(button(root, "Apply import").props.disabled, false)
  } finally { closeImport(app) }
})

test("applying the default import sends changed and identical entries and refreshes the catalog", async () => {
  const preview = importPreviewReply([
    importEntry({ id: "keeps", status: "identical" }),
    importEntry({ id: "docs" }),
    importEntry({ id: "taken", status: "conflict", transport: "http" }),
    importEntry({ id: "broken", status: "invalid", transport: undefined }),
  ])
  const refreshed = { ...catalog(undefined), servers: [...catalog(undefined).servers, { id: "grafana", source: "user", enabled: false, transport: "stdio" }] }
  const { root, app, calls } = await mountImport({ preview, catalogs: [catalog(undefined), refreshed] })
  try {
    await chooseImportFile(root, "{}")
    await button(root, "Apply import").props.onClick()
    await flush()
    const applied = calls.find((call) => call.method === "mcp.import.apply")
    assert.equal(applied.params.source, "user")
    assert.equal(applied.params.expectedVersion, "user:1")
    assert.deepEqual(applied.params.resolutions, [
      { id: "keeps", action: "import" },
      { id: "docs", action: "import" },
      { id: "taken", action: "skip" },
      { id: "broken", action: "skip" },
    ])
    assert.equal(calls.filter((call) => call.method === "mcp.catalog").length, 2)
    assert.match(text(root), /grafana/)
    assert.match(text(root), /Imported 1, replaced 0, renamed 0, skipped 2, unchanged 1\./)
    assert.match(text(root), /Imported entries start disabled/)
    assert.equal(button(root, "Apply import"), undefined)
  } finally { closeImport(app) }
})

test("rename and replace actions are sent as the owner's resolutions", async () => {
  const preview = importPreviewReply([
    importEntry({ id: "docs" }),
    importEntry({ id: "taken", status: "conflict", transport: "http" }),
  ])
  const { root, app, calls } = await mountImport({ preview, catalogs: [catalog(undefined)] })
  try {
    await chooseImportFile(root, "{}")
    const [docs, taken] = importSelects(root)
    enter(docs, "rename"); await flush()
    enter(field(root, "New id"), "docs2"); await flush()
    enter(taken, "replace"); await flush()
    assert.equal(button(root, "Apply import").props.disabled, false)
    await button(root, "Apply import").props.onClick(); await flush()
    const applied = calls.find((call) => call.method === "mcp.import.apply")
    assert.deepEqual(applied.params.resolutions, [
      { id: "docs", action: "rename", newId: "docs2" },
      { id: "taken", action: "replace" },
    ])
    assert.match(text(root), /Imported 0, replaced 1, renamed 1, skipped 0, unchanged 0\./)
  } finally { closeImport(app) }
})

test("a project import applies without confirmation and says where withheld values go", async () => {
  const preview = importPreviewReply([
    importEntry({ id: "keeps", status: "identical" }),
    importEntry({ id: "docs", changes: [{ field: "args", kind: "array" }, { field: "env", kind: "object" }] }),
  ], "project:1")
  const projectCatalog = { ...catalog(undefined), projectRevision: "project:1" }
  const { root, app, calls } = await mountImport({ preview, catalogs: [catalog(undefined), projectCatalog] })
  try {
    await field(root, "Configuration scope").props["onUpdate:modelValue"]("/project")
    await flush()
    assert.match(text(root), /Import into: project \/project/)
    await chooseImportFile(root, "{}")
    const previewCall = calls.find((call) => call.method === "mcp.import.preview")
    assert.equal(previewCall.params.source, "project")
    assert.equal(previewCall.params.cwd, "/project")
    assert.match(text(root), /The workspace file receives each entry's public skeleton/)
    assert.match(text(root), /never to the workspace file\./)
    assert.equal(all(root).find((element) => element.type === "input" && element.props.type === "checkbox"), undefined)
    assert.equal(button(root, "Apply import").props.disabled, false)
    await button(root, "Apply import").props.onClick(); await flush()
    const applied = calls.find((call) => call.method === "mcp.import.apply")
    assert.deepEqual(applied.params.resolutions, [
      { id: "keeps", action: "import" },
      { id: "docs", action: "import" },
    ])
  } finally { closeImport(app) }
})

test("an entry that does not decode offers no action but skip", async () => {
  const preview = importPreviewReply([
    importEntry({ id: "broken", transport: undefined, status: "invalid", error: "broken: type is stdio, http or sse" }),
  ])
  const { root, app, calls } = await mountImport({ preview, catalogs: [catalog(undefined)] })
  try {
    await chooseImportFile(root, "{}")
    assert.equal(importSelects(root).length, 0)
    assert.match(text(root), /This entry does not decode, so it can only be skipped\./)
    assert.equal(button(root, "Apply import").props.disabled, false)
    await button(root, "Apply import").props.onClick(); await flush()
    const applied = calls.find((call) => call.method === "mcp.import.apply")
    assert.deepEqual(applied.params.resolutions, [{ id: "broken", action: "skip" }])
  } finally { closeImport(app) }
})

test("discovery lists known stores and previews a ready one by id", async () => {
  const preview = importPreviewReply([importEntry({ id: "docs" })], "user:1", [], "9f2c", { id: "claude-code:user", client: "claude-code", scope: "user", path: "~/.claude.json" })
  const sources = [
    { id: "claude-code:user", client: "claude-code", scope: "user", path: "~/.claude.json", state: "ready", entries: 2 },
    { id: "codex:user", client: "codex", scope: "user", path: "~/.codex/config.toml", state: "malformed", error: `the file is not valid TOML at line 2 column 1` },
    { id: "omp:user", client: "omp", scope: "user", path: "~/.omp/agent/mcp.json", state: "absent" },
  ]
  const { root, app, calls } = await mountImport({ preview, catalogs: [catalog(undefined)], sources })
  try {
    await button(root, "Discover…").props.onClick()
    await flush()
    const shown = text(root)
    assert.match(shown, /claude-code:user · claude-code · user · ready/)
    assert.match(shown, /~\/.claude\.json/)
    assert.match(shown, /Entries: 2/)
    assert.match(shown, /codex:user · codex · user · malformed/)
    assert.match(shown, /the file is not valid TOML at line 2 column 1/)
    assert.match(shown, /omp:user · omp · user · absent/)
    // Only a ready store offers a preview; the others show their state.
    const previews = all(root).filter((element) => element.type === "button" && text(element) === "Preview")
    assert.equal(previews.length, 1)
    await previews[0].props.onClick()
    await flush()
    const call = calls.find((item) => item.method === "mcp.import.preview")
    assert.equal(call.params.from, "claude-code:user")
    assert.equal("document" in call.params, false)
    assert.equal(call.params.source, "user")
    // The preview names the store it read, in the words the store list uses.
    assert.match(text(root), /From store claude-code:user · claude-code · user · ~\/\.claude\.json · its text stays in the daemon\./)
    assert.match(text(root), /Import preview · user · user:1/)
    // Apply sends the store id and the digest the preview answered, never a
    // document.
    await button(root, "Apply import").props.onClick()
    await flush()
    const applied = calls.find((item) => item.method === "mcp.import.apply")
    assert.equal(applied.params.from, "claude-code:user")
    assert.equal(applied.params.expectedSourceRevision, "9f2c")
    assert.equal("document" in applied.params, false)
    assert.equal(applied.params.expectedVersion, "user:1")
  } finally { closeImport(app) }
})

test("a preview from a store keeps its line when the daemon sends no store row", async () => {
  // A daemon older than the reply's `store` row answers the digest only. The
  // page still knows the id it asked for and names it, without the details.
  const preview = importPreviewReply([importEntry({ id: "docs" })], "user:1", [], "9f2c")
  const sources = [{ id: "claude-code:user", client: "claude-code", scope: "user", path: "~/.claude.json", state: "ready", entries: 2 }]
  const { root, app } = await mountImport({ preview, catalogs: [catalog(undefined)], sources })
  try {
    await button(root, "Discover…").props.onClick()
    await flush()
    await button(root, "Preview").props.onClick()
    await flush()
    const shown = text(root)
    assert.match(shown, /From store claude-code:user · its text stays in the daemon\./)
    assert.equal(shown.includes("~/.claude.json · its text stays"), false)
  } finally { closeImport(app) }
})

test("discovery with no known store says so", async () => {
  const preview = importPreviewReply([importEntry({ id: "docs" })])
  const { root, app } = await mountImport({ preview, catalogs: [catalog(undefined)] })
  try {
    await button(root, "Discover…").props.onClick()
    await flush()
    assert.match(text(root), /No known store was found\./)
  } finally { closeImport(app) }
})

test("a stale source shows preview again and re-reads the same document", async () => {
  const document = '{"mcpServers":{"docs":{"command":"server"}}}'
  const preview = importPreviewReply([importEntry({ id: "docs" })])
  const stale = () => { throw new RpcError(-32000, "settings changed; read the current version before updating", "mcp_settings_conflict") }
  const { root, app, calls } = await mountImport({ preview, catalogs: [catalog(undefined)], apply: stale })
  try {
    await chooseImportFile(root, document)
    await button(root, "Apply import").props.onClick(); await flush()
    assert.match(text(root), /The MCP source changed since this preview; no entry was imported, and an entry it would have replaced may now be disabled\. Preview again before applying\./)
    assert.ok(button(root, "Preview again"))
    await button(root, "Preview again").props.onClick(); await flush()
    const previews = calls.filter((call) => call.method === "mcp.import.preview")
    assert.equal(previews.length, 2)
    assert.equal(previews[1].params.document, document)
    assert.equal(previews[1].params.source, "user")
    assert.equal(button(root, "Preview again"), undefined)
    assert.ok(button(root, "Apply import"))
  } finally { closeImport(app) }
})
