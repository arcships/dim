import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { registerHooks, stripTypeScriptTypes } from "node:module"
import test from "node:test"
import { compileScript, parse } from "vue/compiler-sfc"
import { createRenderer, nextTick, reactive } from "vue"
import { nuxtUiTestPlugin } from "./helpers/nuxt-ui.mjs"

registerHooks({
  resolve(specifier, context, next) {
    if (context.parentURL?.endsWith("PluginSettings.vue") && specifier === "../../stores/connection") {
      return { url: "data:text/javascript,export const useConnectionStore=()=>globalThis.settingsConnection", shortCircuit: true }
    }
    if (specifier === "vue-i18n") return { url: "data:text/javascript,export const useI18n=()=>({locale:{value:'en'}})", shortCircuit: true }
    if (specifier.endsWith(".vue") && !specifier.endsWith("PluginSettings.vue")) return { url: "data:text/javascript,export default { props:['busy','error'], emits:['reload'], setup(props){return ()=>props.error||null} }", shortCircuit: true }
    try { return next(specifier, context) } catch (error) {
      if ((specifier.startsWith("./") || specifier.startsWith("../")) && !/\.(ts|vue)$/.test(specifier)) return next(`${specifier}.ts`, context)
      throw error
    }
  },
  load(url, context, next) {
    if (url.endsWith(".ts")) return { format: "module", source: stripTypeScriptTypes(readFileSync(new URL(url), "utf8"), { mode: "transform" }), shortCircuit: true }
    if (!url.endsWith("PluginSettings.vue")) return next(url, context)
    const { descriptor } = parse(readFileSync(new URL(url), "utf8"))
    return { format: "module", source: stripTypeScriptTypes(compileScript(descriptor, { id: "settings-plugins-test", inlineTemplate: true }).content, { mode: "transform" }), shortCircuit: true }
  },
})

globalThis.Document = class {}
globalThis.ShadowRoot = class {}

const { readPlugins } = await import("../src/rpc/general-settings.ts")
const { default: PluginSettings } = await import("../src/features/settings/PluginSettings.vue")

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
const card = (root, name) => all(root).find((element) => element.type === "section" && text(element).startsWith(name))
const field = (root, label) => all(root).find((element) => element.type === "label" && text(element) === label)?.children.find((element) => element.type === "input" || element.type === "select")
const enter = (element, value) => {
  if (element.props["onUpdate:modelValue"]) element.props["onUpdate:modelValue"](value)
  else element.props.onChange({ target: { value } })
}
const submit = (root) => all(root).find((element) => element.type === "form" && text(element).includes("Install")).props.onSubmit({ preventDefault() {} })

/** `plugin.list` as PluginManager §3.6 lists it: an optional plugin Dim did
 * not install, and a default plugin that carries neither field. */
function plugins() {
  return [
    { name: "light-ocr", use: null, off: true, locked: false, candidates: [], optional: true },
    { name: "web", use: "default", off: false, locked: false, candidates: [{ id: "default", kinds: ["tool"] }], status: "running" },
  ]
}

function mount() {
  const calls = []
  globalThis.window = { confirm: () => true }
  globalThis.settingsConnection = reactive({
    phase: "connected",
    supports: () => true,
    client: { call: async (method, params, read, timeoutMs) => {
      calls.push({ method, params, timeoutMs })
      return read(method === "plugin.list" ? plugins() : {})
    } },
  })
  const root = node("root")
  const app = renderer.createApp(PluginSettings)
  app.use(nuxtUiTestPlugin)
  app.mount(root)
  return { root, app, calls }
}

function unmount(app) {
  app.unmount()
  delete globalThis.settingsConnection
  delete globalThis.window
}

test("the optional item reads as optional and a plain item does not", () => {
  const [optional, plain] = readPlugins(plugins())
  assert.equal(optional.optional, true)
  assert.deepEqual(optional.candidates, [])
  assert.equal(optional.off, true)
  assert.equal(plain.optional, undefined)
  assert.equal(plain.status, "running")
})

test("an uninstalled optional plugin offers Install in place of Enable", async () => {
  const { root, app } = mount()
  try {
    await flush()
    const optional = card(root, "light-ocr")
    assert.ok(optional, "the optional plugin has its card")
    assert.ok(button(optional, "Install"), "the card offers Install")
    assert.equal(button(optional, "Enable / use"), undefined)
    const plain = card(root, "web")
    assert.ok(button(plain, "Enable / use"), "a normal plugin still offers Enable")
    assert.equal(button(plain, "Install"), undefined)
  } finally { unmount(app) }
})

test("installing the optional plugin calls plugin.install with the 15-minute timeout", async () => {
  const { root, app, calls } = mount()
  try {
    await flush()
    calls.length = 0
    await button(card(root, "light-ocr"), "Install").props.onClick()
    await flush()
    const install = calls.find((call) => call.method === "plugin.install")
    assert.ok(install, "plugin.install was called")
    assert.deepEqual(install.params, { source: { optional: "light-ocr" } })
    assert.equal(install.timeoutMs, 900_000)
  } finally { unmount(app) }
})

test("a path install keeps its 180-second timeout", async () => {
  const { root, app, calls } = mount()
  try {
    await flush()
    calls.length = 0
    enter(field(root, "Source"), "/p/my-tool")
    await flush()
    await submit(root)
    await flush()
    const install = calls.find((call) => call.method === "plugin.install")
    assert.ok(install, "plugin.install was called")
    assert.deepEqual(install.params, { source: { path: "/p/my-tool" }, enable: false })
    assert.equal(install.timeoutMs, 180_000)
  } finally { unmount(app) }
})
