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
    if (specifier === "../../stores/archive") return { url: "data:text/javascript,export const useArchiveStore=()=>globalThis.archiveHarness.archive", shortCircuit: true }
    if (specifier === "../../stores/connection") return { url: "data:text/javascript,export const useConnectionStore=()=>globalThis.archiveHarness.connection", shortCircuit: true }
    if (specifier === "../../stores/sessions") return { url: "data:text/javascript,export const useSessionsStore=()=>globalThis.archiveHarness.sessions;export const workspaceName=cwd=>{const t=cwd.replace(/\\/+$/,'');return t.split('/').at(-1)||cwd}", shortCircuit: true }
    if (specifier === "vue-i18n") return { url: "data:text/javascript,export const useI18n=()=>({locale:{value:'en'}})", shortCircuit: true }
    if (specifier === "vue-router") return { url: `data:text/javascript,import{h}from'${import.meta.resolve("vue")}';export const useRouter=()=>({push:async to=>globalThis.archiveHarness.navigations.push(to)});export const RouterLink={props:['to'],setup:(props,{slots})=>()=>h('a',{to:props.to},slots.default?.())}`, shortCircuit: true }
    try {
      return next(specifier, context)
    } catch (error) {
      if (specifier.startsWith(".") && !/\.(ts|vue)$/.test(specifier)) return next(`${specifier}.ts`, context)
      throw error
    }
  },
  load(url, context, next) {
    if (url.endsWith(".ts")) return { format: "module", source: stripTypeScriptTypes(readFileSync(new URL(url), "utf8"), { mode: "transform" }), shortCircuit: true }
    if (!url.endsWith(".vue")) return next(url, context)
    const { descriptor } = parse(readFileSync(new URL(url), "utf8"))
    return { format: "module", source: stripTypeScriptTypes(compileScript(descriptor, { id: "settings-archive", inlineTemplate: true }).content, { mode: "transform" }), shortCircuit: true }
  },
})
const { default: ArchivedSettings } = await import("../src/features/settings/ArchivedSettings.vue")
const { RpcError } = await import("../src/rpc/client.ts")
const node = (type, text = "") => ({ type, text, children: [], props: {}, parent: undefined, addEventListener() {}, getRootNode() { return {} } })
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
const card = (root, name) => all(root).find(element => element.type === "section" && text(element).includes(name))
const action = (root, label) => all(root).find(element => ["button", "a"].includes(element.type) && text(element) === label)
const search = root => all(root).find(element => element.type === "input")
const workspaceOptions = root => all(root).filter(element => element.type === "option").map(text)

function fakeArchive(hidden) {
  const store = reactive({
    hidden,
    has: id => store.hidden.includes(id),
    restore: id => { store.hidden = store.hidden.filter(item => item !== id); return true },
    drop: ids => { store.hidden = store.hidden.filter(id => !ids.includes(id)); return true },
  })
  return store
}

const row = extra => ({ cwd: "/work/dim", model: { providerId: "fixture", modelId: "model-a" }, createdAt: 1, updatedAt: 2, sequence: 1, running: false, title: null, titleSource: null, titleRevision: 0, ...extra })

function mount(t, call, { hidden = [], supports = () => true, confirm = () => true } = {}) {
  globalThis.window = { confirm }
  globalThis.archiveHarness = {
    archive: fakeArchive([...hidden]),
    forgotten: [],
    navigations: [],
    connection: reactive({ phase: "connected", supports, client: { call: async (method, params, read) => read(await call(method, params)) } }),
    sessions: { forget(id) { globalThis.archiveHarness.forgotten.push(id) } },
  }
  const root = node("root"), app = renderer.createApp(ArchivedSettings).use(nuxtUiTestPlugin)
  app.mount(root)
  t.after(() => { app.unmount(); delete globalThis.window; delete globalThis.archiveHarness })
  return { root, harness: globalThis.archiveHarness }
}

test("lists only archived rows, including closed ones, and filters them on the client", async t => {
  const calls = []
  const { root } = mount(t, async (method, params) => {
    assert.equal(method, "session.summaries")
    calls.push(params)
    return [
      row({ sessionId: "alpha", preview: "Alpha", cwd: "/work/one" }),
      row({ sessionId: "beta", preview: "Beta", cwd: "/work/two", closed: true }),
      row({ sessionId: "gamma", preview: "Gamma", cwd: "/work/one", running: true }),
      row({ sessionId: "delta", preview: "Delta", cwd: "/work/three" }),
    ]
  }, { hidden: ["alpha", "beta", "gamma", "gone"] })
  await flush()

  assert.deepEqual(calls, [{ includeClosed: true }])
  assert.match(text(root), /Alpha/)
  assert.match(text(root), /Beta/)
  assert.match(text(root), /Gamma/)
  assert.doesNotMatch(text(root), /Delta/, "a Session this browser did not hide is not archived")
  assert.match(text(root), /1 archived entries are not stored by this daemon/)

  assert.ok(action(card(root, "Alpha"), "Restore"))
  assert.equal(action(card(root, "Beta"), "Open"), undefined, "a closed Session is not offered Open")
  assert.match(text(card(root, "Beta")), /Closed in this daemon/)
  assert.equal(action(card(root, "Gamma"), "Delete permanently").props.disabled, true, "a running Session is not deleted")

  assert.deepEqual(workspaceOptions(root), ["All workspaces", "one — /work/one", "two — /work/two"])
  search(root).props["onUpdate:modelValue"]("beta")
  await flush()
  assert.doesNotMatch(text(root), /Alpha/)
  assert.match(text(root), /Beta/)
  search(root).props["onUpdate:modelValue"]("")
  await flush()
  assert.match(text(root), /Alpha/)
})

test("restoring and opening remove the marker without a server call, and Remove unavailable deletes no Session", async t => {
  const calls = []
  const { root, harness } = mount(t, async (method, params) => {
    calls.push([method, params])
    return [row({ sessionId: "alpha", preview: "Alpha" }), row({ sessionId: "delta", preview: "Delta" })]
  }, { hidden: ["alpha", "gone"] })
  await flush()

  await action(card(root, "Alpha"), "Open").props.onClick()
  await flush()
  assert.deepEqual(harness.navigations, [{ name: "chat-session", params: { sessionId: "alpha" } }])
  assert.deepEqual(harness.archive.hidden, ["gone"])
  assert.doesNotMatch(text(root), /Alpha/)

  await action(root, "Remove unavailable").props.onClick()
  await flush()
  assert.deepEqual(harness.archive.hidden, [])
  assert.doesNotMatch(text(root), /not stored by this daemon/)
  assert.deepEqual(calls.map(([method]) => method), ["session.summaries"], "the browser list never calls session.delete")
})

test("deleting one row is confirmed and reported, and a bulk failure keeps the row that was not deleted", async t => {
  const deletes = []
  let confirmations = 0
  const { root, harness } = mount(t, async (method, params) => {
    if (method === "session.summaries") return [row({ sessionId: "alpha", preview: "Alpha" }), row({ sessionId: "beta", preview: "Beta" }), row({ sessionId: "delta", preview: "Delta" })]
    assert.equal(method, "session.delete")
    deletes.push(params)
    if (params.sessionId === "beta") throw new RpcError(-32000, "Session is in use", "session_active")
    return { deleted: true }
  }, {
    hidden: ["alpha", "beta", "missing"],
    confirm: () => { confirmations += 1; return true },
  })
  await flush()

  await action(card(root, "Alpha"), "Delete permanently").props.onClick()
  await flush()
  assert.equal(confirmations, 1)
  assert.deepEqual(deletes[0], { sessionId: "alpha", requireInactive: true })
  assert.deepEqual(harness.archive.hidden, ["beta", "missing"])
  assert.deepEqual(harness.forgotten, ["alpha"])
  assert.doesNotMatch(text(root), /Alpha/)
  assert.match(text(root), /Deleted the archived session/)

  await action(root, "Delete all archived").props.onClick()
  await flush()
  assert.equal(confirmations, 2)
  assert.deepEqual(deletes.slice(1), [{ sessionId: "beta", requireInactive: true }], "only the archived row that is left is a target")
  assert.deepEqual(harness.archive.hidden, ["beta", "missing"], "the failed row keeps its marker")
  assert.match(text(root), /Session is in use and was not deleted/)
  assert.match(text(root), /Deleted 0 of 1 archived sessions/)
})

test("an App Server without includeClosed is read once without it", async t => {
  const calls = []
  const { root } = mount(t, async (method, params) => {
    calls.push(params)
    if (calls.length === 1) throw new RpcError(-32602, "Invalid request parameters", "invalid_params")
    return [row({ sessionId: "alpha", preview: "Alpha" })]
  }, { hidden: ["alpha"] })
  await flush()

  assert.deepEqual(calls, [{ includeClosed: true }, {}])
  assert.match(text(root), /Alpha/)
})

test("without a connection that serves session.delete the destructive actions are disabled", async t => {
  const { root } = mount(t, async () => [row({ sessionId: "alpha", preview: "Alpha" })], { hidden: ["alpha"], supports: () => false })
  await flush()
  assert.equal(action(card(root, "Alpha"), "Delete permanently").props.disabled, true)
  assert.equal(action(root, "Delete all archived").props.disabled, true)
})
