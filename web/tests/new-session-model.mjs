import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { registerHooks, stripTypeScriptTypes } from "node:module"
import test from "node:test"
import { compileScript, parse } from "vue/compiler-sfc"
import { createRenderer, nextTick, reactive } from "vue"
import { nuxtUiTestPlugin } from "./helpers/nuxt-ui.mjs"

globalThis.Document = class {}
globalThis.ShadowRoot = class {}
globalThis.ResizeObserver = class {
  observe() {}
  disconnect() {}
}
globalThis.matchMedia = () => ({ matches: true })

const stores = {
  chat: "useChatStore",
  composer: "useComposerStore",
  connection: "useConnectionStore",
  models: "useModels",
  sessions: "useSessionsStore",
}
/** The new-Session page and the composer it renders; every other SFC renders nothing. */
const components = /\/(NewSessionPage|ComposerPanel|ComposerModelMenu)\.vue$/
registerHooks({
  resolve(specifier, context, next) {
    const store = stores[specifier.split("/").at(-1)]
    if (store !== undefined && specifier.startsWith("../../stores/")) {
      const source = `export const ${store}=()=>globalThis.newSessionStores.${specifier.split("/").at(-1)}${store === "useSessionsStore" ? ";export const workspaceName=cwd=>cwd" : ""}`
      return { url: `data:text/javascript,${encodeURIComponent(source)}`, shortCircuit: true }
    }
    if (specifier === "vue-i18n") return { url: "data:text/javascript,export const useI18n=()=>({t:key=>key,locale:{value:'en'}})", shortCircuit: true }
    if (specifier === "vue-router") return { url: "data:text/javascript,export const useRoute=()=>({query:{}});export const useRouter=()=>({push:async()=>{}})", shortCircuit: true }
    if (specifier.endsWith(".vue") && !components.test(specifier)) return { url: "data:text/javascript,export default {render:()=>null}", shortCircuit: true }
    try {
      return next(specifier, context)
    } catch (error) {
      if (specifier.startsWith(".") && !/\.(ts|vue)$/.test(specifier)) return next(`${specifier}.ts`, context)
      throw error
    }
  },
  load(url, context, next) {
    if (url.endsWith(".ts")) return { format: "module", source: stripTypeScriptTypes(readFileSync(new URL(url), "utf8"), { mode: "transform" }), shortCircuit: true }
    if (!components.test(url)) return next(url, context)
    const { descriptor } = parse(readFileSync(new URL(url), "utf8"))
    return { format: "module", source: stripTypeScriptTypes(compileScript(descriptor, { id: "new-session-model-test", inlineTemplate: true }).content, { mode: "transform" }), shortCircuit: true }
  },
})

const { default: NewSessionPage } = await import("../src/features/chat/NewSessionPage.vue")
const node = (type, text = "") => ({
  type, text, children: [], props: {}, parent: undefined, style: {}, value: "",
  get scrollHeight() {
    let element = this
    while (element.parent !== undefined) element = element.parent
    return element.type === "root" ? 40 : 0
  },
  addEventListener() {}, focus() {}, scrollTo() {}, getRootNode() { return {} },
  setSelectionRange() {},
})
const renderer = createRenderer({
  createElement: (type) => node(type), createText: (text) => node("text", text), createComment: (text) => node("comment", text),
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
const all = (element) => [element, ...element.children.flatMap(all)]
const text = (element) => (element.type === "comment" ? "" : element.text) + element.children.map(text).join("")
const button = (root, label) => all(root).find((element) => element.type === "button" && (text(element) === label || element.props["aria-label"] === label))
const label = (root) => text(all(root).find((element) => element.props.class === "model-trigger-label"))
const follow = (root) => all(root).find((element) => element.type === "button" && element.props.role === "option" && text(element).includes("composer.followDefaults"))
const workspace = (root, cwd) => all(root).find((element) => element.type === "button" && element.props.role === "option" && text(element).includes(cwd))
const flush = async () => { for (let i = 0; i < 12; i++) { await Promise.resolve(); await nextTick() } }
const click = (element) => element.props.onClick({ stopPropagation() {} })
const ALPHA = { providerId: "fixture", modelId: "alpha" }
const BETA = { providerId: "fixture", modelId: "beta" }

function mount(t, resolve, workspaces = [{ cwd: "/one", name: "one" }]) {
  const previews = [], creations = [], queries = []
  globalThis.newSessionStores = {
    chat: { open() {}, async submit() {} },
    connection: reactive({ hello: undefined, supports: (method) => method === "session.model.preview" }),
    models: {
      available: true, error: "", loading: false,
      providers: [{ providerId: "fixture", name: "Fixture", models: [{ id: "alpha", name: "Alpha" }, { id: "beta", name: "Beta" }] }],
      async load(query) { queries.push(query) },
      clear() {},
      nameOf: (model) => model.modelId === "alpha" ? "Alpha" : model.modelId === "beta" ? "Beta" : model.modelId,
      async preview(cwd) { previews.push(cwd); return resolve(cwd) },
    },
    sessions: {
      workspaces,
      async create(cwd, model) { creations.push({ cwd, model }); return "created" },
    },
    composer: reactive({ sendMode: "steer", alternate: () => "queue" }),
  }
  const root = node("root")
  const app = renderer.createApp(NewSessionPage)
  app.use(nuxtUiTestPlugin)
  app.mount(root)
  t.after(() => app.unmount())
  return { root, previews, creations, queries }
}

async function send(root, words = "hello") {
  const field = all(root).find((element) => element.type === "textarea")
  field.props["onUpdate:modelValue"](words)
  await flush()
  await click(button(root, "composer.send"))
  await flush()
}

test("the composer names the model creation would adopt and keeps following defaults separate", async (t) => {
  const { root, creations, queries } = mount(t, async () => ALPHA)
  await flush()
  assert.equal(label(root), "Alpha", "the resolved model is named, not the default")
  assert.deepEqual(queries, [{ providerId: "fixture", modelId: "alpha" }], "the name comes from exact-model discovery")
  assert.equal(follow(root).props["aria-selected"], true)
  assert.match(text(follow(root)), /Alpha/)
  await send(root)
  assert.deepEqual(creations, [{ cwd: "/one", model: undefined }], "following defaults sends no model")
})

test("a model chosen for one Session overrides the preview and following defaults restores it", async (t) => {
  const { root, creations } = mount(t, async () => ALPHA, [{ cwd: "/one", name: "one" }, { cwd: "/two", name: "two" }])
  await flush()
  await click(button(root, "Beta"))
  await flush()
  assert.equal(label(root), "Beta")
  assert.equal(follow(root).props["aria-selected"], false)
  assert.match(text(follow(root)), /Alpha$/, "the default row names the model it restores, not the choice")
  assert.doesNotMatch(text(follow(root)), /Beta/)
  await click(workspace(root, "/two"))
  await flush()
  assert.equal(label(root), "Beta", "reading another directory does not overwrite the choice")
  await send(root)
  assert.deepEqual(creations, [{ cwd: "/two", model: BETA }], "a choice applies to this Session only")
  await click(follow(root))
  await flush()
  assert.equal(label(root), "Alpha")
  assert.equal(follow(root).props["aria-selected"], true)
  await send(root)
  assert.equal(creations[1].model, undefined, "following defaults again removes the explicit model")
})

test("resolving, an unavailable read and no usable model are told apart", async (t) => {
  let release
  const pending = new Promise((resolve) => { release = resolve })
  const { root, previews } = mount(t, async () => pending)
  await flush()
  assert.equal(label(root), "composer.modelResolving")
  release(undefined)
  await flush()
  assert.equal(label(root), "composer.noModels", "a resolved nothing is not a failure")
  assert.equal(follow(root).props["aria-selected"], true)
})

test("a failed read keeps its own label and offers a retry", async (t) => {
  let attempt = 0
  const { root, previews } = mount(t, async () => {
    if (++attempt === 1) throw new Error("provider reload")
    return ALPHA
  })
  await flush()
  assert.equal(label(root), "composer.modelUnavailable")
  const retry = button(root, "composer.modelRetry")
  assert(retry !== undefined, "the menu offers a retry")
  await click(retry)
  await flush()
  assert.equal(previews.length, 2)
  assert.equal(label(root), "Alpha")
})

test("a workspace switch shows only that workspace's model, and a stale reply is ignored", async (t) => {
  let release
  const pending = new Promise((resolve) => { release = resolve })
  const { root, previews } = mount(t, async (cwd) => cwd === "/one" ? pending : BETA, [
    { cwd: "/one", name: "one" },
    { cwd: "/two", name: "two" },
  ])
  await flush()
  await click(workspace(root, "/two"))
  await flush()
  assert.deepEqual(previews, ["/one", "/two"])
  assert.equal(label(root), "Beta")
  release(ALPHA)
  await flush()
  assert.equal(label(root), "Beta", "the earlier workspace's reply does not replace this one")
})
