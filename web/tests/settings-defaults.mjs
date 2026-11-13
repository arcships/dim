import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { registerHooks, stripTypeScriptTypes } from "node:module"
import test from "node:test"
import { compileScript, parse } from "vue/compiler-sfc"
import { createRenderer, nextTick, reactive } from "vue"
import { nuxtUiTestPlugin } from "./helpers/nuxt-ui.mjs"

globalThis.Document = class {}
globalThis.ShadowRoot = class {}
registerHooks({
  resolve(specifier, context, next) {
    if (specifier === "../../stores/connection") return { url: "data:text/javascript,export const useConnectionStore=()=>globalThis.defaultsConnection", shortCircuit: true }
    if (specifier === "../../stores/sessions") return { url: "data:text/javascript,export const useSessionsStore=()=>({workspaces:[{cwd:'/project',name:'Project'}]})", shortCircuit: true }
    if (specifier === "../../stores/models") return { url: "data:text/javascript,export const useModelsStore=()=>({providers:[{providerId:'fixture',name:'Fixture',models:[{id:'a',name:'A'},{id:'b',name:'B'}]}],load:async()=>{},error:''})", shortCircuit: true }
    if (specifier === "vue-i18n") return { url: "data:text/javascript,export const useI18n=()=>({locale:{value:'en'}})", shortCircuit: true }
    if (specifier === "vue-router" && context.parentURL?.endsWith("ArchivedSettings.vue")) return { url: "data:text/javascript,export const RouterLink={inheritAttrs:false,setup:(_,ctx)=>()=>ctx.slots.default?.()}", shortCircuit: true }
    try { return next(specifier, context) } catch (error) {
      if (specifier.startsWith(".") && !/\.(ts|vue)$/.test(specifier)) return next(`${specifier}.ts`, context)
      throw error
    }
  },
  load(url, context, next) {
    if (url.endsWith(".ts")) return { format: "module", source: stripTypeScriptTypes(readFileSync(new URL(url), "utf8"), { mode: "transform" }), shortCircuit: true }
    if (!url.endsWith(".vue")) return next(url, context)
    const { descriptor } = parse(readFileSync(new URL(url), "utf8"))
    return { format: "module", source: stripTypeScriptTypes(compileScript(descriptor, { id: "settings-defaults", inlineTemplate: true }).content, { mode: "transform" }), shortCircuit: true }
  },
})
const { default: PermissionsSettings } = await import("../src/features/settings/PermissionsSettings.vue")
const { default: AgentSettings } = await import("../src/features/settings/AgentSettings.vue")
const { default: ArchivedSettings } = await import("../src/features/settings/ArchivedSettings.vue")
const { RpcError } = await import("../src/rpc/client.ts")
const node = (type, text = "") => ({ type, text, children: [], props: {}, parent: undefined, options: [], selectedIndex: -1, addEventListener() {}, getRootNode() { return {} } })
const renderer = createRenderer({
  createElement: type => node(type), createText: text => node("text", text), createComment: text => node("comment", text),
  patchProp(element, key, _, value) { element.props[key] = value },
  setText(element, text) { element.text = text }, setElementText(element, text) { element.text = text; element.children = [] },
  parentNode: element => element.parent, nextSibling: element => element.parent?.children[element.parent.children.indexOf(element) + 1],
  insert(element, parent, anchor) { if (element.parent) element.parent.children.splice(element.parent.children.indexOf(element), 1); parent.children.splice(anchor ? parent.children.indexOf(anchor) : parent.children.length, 0, element); element.parent = parent },
  remove(element) { if (element.parent) element.parent.children.splice(element.parent.children.indexOf(element), 1); element.parent = undefined },
})
const all = element => [element, ...element.children.flatMap(all)]
const text = element => (element.type === "comment" ? "" : element.text) + element.children.map(text).join("")
const flush = async () => { for (let i = 0; i < 12; i++) { await Promise.resolve(); await nextTick() } }
const button = (root, label) => all(root).find(el => el.type === "button" && text(el) === label)
const field = (root, label) => all(root).find(el => el.type === "label" && text(el).startsWith(label)).children.find(el => el.type === "select")
const submit = async (root, label) => { await all(root).find(el => el.type === "form" && text(el).includes(label)).props.onSubmit({ preventDefault() {} }); await flush() }
function mount(t, component, call, supports = () => true) {
  globalThis.window = { confirm: () => true }
  globalThis.defaultsConnection = reactive({ phase: "connected", supports, client: { call: async (method, params, read) => read(await call(method, params)) } })
  const root = node("root"), app = renderer.createApp(component).use(nuxtUiTestPlugin)
  app.mount(root)
  t.after(() => { app.unmount(); delete globalThis.window; delete globalThis.defaultsConnection })
  return root
}

test("saved-session pagination waits for Search when workspace or text filters change", async t => {
  const reads = []
  let fail = false
  const root = mount(t, ArchivedSettings, async (method, params) => {
    assert.equal(method, "session.catalog")
    reads.push(params)
    if (fail) throw new Error("Search failed")
    const id = `${params.cwd ?? "all"}:${params.query ?? "any"}:${params.cursor ?? "first"}`
    return { sessions: [{ sessionId: id, cwd: params.cwd ?? "/other", model: { providerId: "fixture", modelId: "a" }, createdAt: 1, updatedAt: 2, sequence: 1, running: false, preview: id, title: null, titleSource: null, titleRevision: 0 }], nextCursor: params.cursor ? null : "next" }
  })
  await flush()
  const more = () => button(root, "Load more")
  const workspace = field(root, "Workspace filter")
  const query = all(root).find(el => el.type === "input")
  assert.equal(more().props.disabled, false)
  workspace.props["onUpdate:modelValue"]("/project"); await flush()
  assert.equal(more().props.disabled, true)
  await more().props.onClick(); await flush()
  assert.equal(reads.length, 1, "a changed filter must not page through the previous workspace")
  fail = true
  await submit(root, "Search")
  assert.match(text(root), /Search failed/)
  assert.equal(more().props.disabled, true)
  fail = false
  await submit(root, "Search")
  assert.deepEqual(reads.at(-1), { limit: 50, withPreview: true, cwd: "/project" })
  assert.doesNotMatch(text(root), /all:any:first/)
  assert.match(text(root), /\/project:any:first/)
  query.props["onUpdate:modelValue"]("needle"); await flush()
  assert.equal(more().props.disabled, true)
  query.props["onUpdate:modelValue"]("  "); await flush()
  assert.equal(more().props.disabled, false, "unchanged effective filters allow pagination")
  query.props["onUpdate:modelValue"](" needle "); await flush()
  await submit(root, "Search")
  assert.deepEqual(reads.at(-1), { limit: 50, withPreview: true, cwd: "/project", query: "needle" })
  await more().props.onClick(); await flush()
  assert.deepEqual(reads.at(-1), { limit: 50, withPreview: true, cwd: "/project", query: "needle", cursor: "next" })
  assert.match(text(root), /\/project:needle:first/)
  assert.match(text(root), /\/project:needle:next/)
  assert.equal(more(), undefined)
})

test("isolation modes preserve inactive network, reset both flags and retain conflicting drafts without retry", async t => {
  let document = { sandbox: true, network: true }, revision = "exec:1", conflict = false, failRead = false
  const writes = []
  const root = mount(t, PermissionsSettings, async (method, params) => {
    if (method === "permission.rules") return { revision: "rules:1", commandRules: [] }
    if (method === "host.execution.backend") return { available: false, reason: "fixture backend unavailable" }
    if (method === "settings.update") {
      writes.push(params)
      if (conflict) throw new RpcError(-32000, "settings conflict", "settings_conflict")
      assert.equal(params.expectedRevision, revision)
      document = params.document
      revision += "+"
    } else {
      assert.equal(method, "settings.read")
      if (failRead) throw new Error("Settings read failed")
    }
    return { revision, document }
  })
  await flush()
  assert.match(text(root), /fixture backend unavailable/)
  const mode = field(root, "Mode")
  mode.props["onUpdate:modelValue"]("off"); await flush()
  await submit(root, "Save isolation settings")
  assert.deepEqual(writes[0].document, { sandbox: false, network: true })
  globalThis.window.confirm = () => false
  await button(root, "Reset to default").props.onClick(); await flush()
  assert.equal(writes.length, 1)
  globalThis.window.confirm = () => true
  await button(root, "Reset to default").props.onClick(); await flush()
  assert.deepEqual(writes[1].document, { sandbox: false, network: false })
  mode.props["onUpdate:modelValue"]("isolated"); await flush()
  await submit(root, "Save isolation settings")
  assert.deepEqual(writes[2].document, { sandbox: true, network: false })
  mode.props["onUpdate:modelValue"]("network"); await flush()
  conflict = true
  await submit(root, "Save isolation settings")
  assert.deepEqual(writes[3].document, { sandbox: true, network: true })
  assert.equal(writes.length, 4)
  assert.equal(button(root, "Save isolation settings").props.disabled, true)
  assert.equal(button(root, "Reset to default").props.disabled, true)
  assert.match(text(root), /Reload before saving again/)
  await submit(root, "Save isolation settings")
  await button(root, "Reset to default").props.onClick(); await flush()
  assert.equal(writes.length, 4, "neither submit nor reset may replay a stale revision")
  globalThis.window.confirm = () => false
  await button(root, "Reload").props.onClick(); await flush()
  assert.equal(button(root, "Save isolation settings").props.disabled, true)
  globalThis.window.confirm = () => true
  failRead = true
  await button(root, "Reload").props.onClick(); await flush()
  assert.match(text(root), /Settings read failed/)
  assert.equal(button(root, "Save isolation settings").props.disabled, true)
  failRead = false
  revision = "exec:remote"
  await button(root, "Reload").props.onClick(); await flush()
  assert.equal(button(root, "Reset to default").props.disabled, false)
  assert.equal(button(root, "Save isolation settings").props.disabled, true, "reload replaces the conflicting draft")
  assert.deepEqual(writes.map(write => [write.source, write.scope]), Array(4).fill(["execution", "user"]))
  conflict = false
  mode.props["onUpdate:modelValue"]("network"); await flush()
  await submit(root, "Save isolation settings")
  assert.equal(writes[4].expectedRevision, "exec:remote")
  assert.deepEqual(writes[4].document, { sandbox: true, network: true })
})

test("enabling a sandbox with network adds isolation, while removing sandbox or network restrictions requires confirmation", async t => {
  let document = { sandbox: false, network: false }
  const writes = [], confirmations = []
  const root = mount(t, PermissionsSettings, async (method, params) => {
    if (method === "permission.rules") return { revision: "rules:1", commandRules: [] }
    if (method === "host.execution.backend") return { available: true }
    if (method === "settings.update") { writes.push(params); document = params.document }
    else assert.equal(method, "settings.read")
    return { revision: "exec:1", document }
  })
  await flush()
  globalThis.window.confirm = message => { confirmations.push(message); return false }
  const mode = field(root, "Mode")
  mode.props["onUpdate:modelValue"]("network"); await flush()
  await submit(root, "Save isolation settings")
  assert.deepEqual(writes.map(write => write.document), [{ sandbox: true, network: true }])
  assert.equal(confirmations.length, 0)
  mode.props["onUpdate:modelValue"]("off"); await flush()
  await submit(root, "Save isolation settings")
  assert.equal(writes.length, 1)
  assert.equal(confirmations.length, 1)
  mode.props["onUpdate:modelValue"]("isolated"); await flush()
  await submit(root, "Save isolation settings")
  assert.deepEqual(writes[1].document, { sandbox: true, network: false })
  assert.equal(confirmations.length, 1)
  mode.props["onUpdate:modelValue"]("network"); await flush()
  await submit(root, "Save isolation settings")
  assert.equal(writes.length, 2)
  assert.equal(confirmations.length, 2)
})

test("unsupported isolation does not display an invented default", async t => {
  const root = mount(t, PermissionsSettings, async () => ({ revision: "rules:1", commandRules: [] }), method => method !== "settings.read")
  await flush()
  assert.match(text(root), /Process isolation settings are not available/)
  assert.equal(button(root, "Save isolation settings"), undefined)
})

test("default-model scope changes preserve cancelled or failed drafts and never scope user-wide controls", async t => {
  const writes = []
  let fail = false
  const root = mount(t, AgentSettings, async (method, params) => {
    if (method === "provider.selection") {
      if (fail) throw new Error("project unavailable")
      return { selection: { provider_id: "fixture", model_id: "a", version: 7 } }
    }
    if (method === "collaboration.model_policy") return { revision: "policy:1", policy: { mode: "agent" } }
    if (method === "host.exec_shell.get") return { revision: "shell:1", shells: [] }
    writes.push([method, params])
    if (method === "provider.select") return { provider_id: params.providerId, model_id: params.modelId, version: 8 }
    if (method === "collaboration.set_model_policy") return { revision: "policy:2" }
    throw new Error(`unexpected ${method}`)
  })
  await flush()
  const scope = field(root, "Default model scope"), model = field(root, "Model"), policy = field(root, "Model policy")
  model.props["onUpdate:modelValue"](JSON.stringify(["fixture", "b"])); await flush()
  globalThis.window.confirm = () => false
  await scope.props["onUpdate:modelValue"]("/project"); await flush()
  assert.equal(scope.props.value, "__user__")
  globalThis.window.confirm = () => true
  fail = true
  await scope.props["onUpdate:modelValue"]("/project"); await flush()
  assert.equal(scope.props.value, "__user__")
  assert.match(text(root), /project unavailable/)
  await submit(root, "Save defaults")
  assert.equal(writes[0][1].cwd, undefined)
  fail = false
  await scope.props["onUpdate:modelValue"]("/project"); await flush()
  model.props["onUpdate:modelValue"](JSON.stringify(["fixture", "b"]))
  policy.props["onUpdate:modelValue"]("inherit"); await flush()
  await submit(root, "Save defaults")
  assert.deepEqual(writes[1], ["provider.select", { cwd: "/project", providerId: "fixture", modelId: "b", expectedVersion: 7 }])
  assert.deepEqual(writes[2], ["collaboration.set_model_policy", { expectedRevision: "policy:1", policy: { mode: "inherit" } }])
})
