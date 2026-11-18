import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { registerHooks, stripTypeScriptTypes } from "node:module"
import test from "node:test"
import { compileScript, parse } from "vue/compiler-sfc"
import { createRenderer, nextTick, reactive } from "vue"
import { nuxtUiTestPlugin } from "./helpers/nuxt-ui.mjs"

globalThis.Document = class {}
globalThis.ShadowRoot = class {}

const settingsComponent = (url) => /\/(ProviderModels)\.vue$/.test(url)
registerHooks({
  resolve(specifier, context, next) {
    if (specifier === "../../stores/connection") {
      return { url: "data:text/javascript,export const useConnectionStore=()=>globalThis.modelsConnection", shortCircuit: true }
    }
    if (specifier === "../../stores/models") {
      return { url: "data:text/javascript,export const useModels=()=>({load:async()=>{}})", shortCircuit: true }
    }
    if (specifier === "vue-i18n") {
      return {
        url: "data:text/javascript,export const useI18n=()=>({t:(key,args)=>args?[key,...Object.values(args).map(String)].join(' '):key,locale:{value:'en'}})",
        shortCircuit: true,
      }
    }
    if (specifier.endsWith(".vue") && !settingsComponent(specifier)) {
      return { url: "data:text/javascript,export default {setup(props,{expose}){expose({open:async()=>true});return ()=>null}}", shortCircuit: true }
    }
    try { return next(specifier, context) } catch (error) {
      if ((specifier.startsWith("./") || specifier.startsWith("../")) && !/\.(ts|vue)$/.test(specifier)) return next(`${specifier}.ts`, context)
      throw error
    }
  },
  load(url, context, next) {
    if (url.endsWith(".ts")) return { format: "module", source: stripTypeScriptTypes(readFileSync(new URL(url), "utf8"), { mode: "transform" }), shortCircuit: true }
    if (!settingsComponent(url)) return next(url, context)
    const { descriptor } = parse(readFileSync(new URL(url), "utf8"))
    return { format: "module", source: stripTypeScriptTypes(compileScript(descriptor, { id: "provider-models-test", inlineTemplate: true }).content, { mode: "transform" }), shortCircuit: true }
  },
})

const { default: ProviderModels } = await import("../src/features/settings/ProviderModels.vue")
const { readManagement } = await import("../src/rpc/management.ts")
const node = (type, text = "") => ({
  type, text, children: [], props: {}, parent: undefined, value: "", options: [], selectedIndex: -1,
  addEventListener() {}, getRootNode() { return {} },
})
const renderer = createRenderer({
  createElement(type) {
    const element = node(type)
    if (type === "dialog") {
      element.showModal = () => { element.open = true }
      element.close = () => { element.open = false }
    }
    return element
  },
  createText: (text) => node("text", text), createComment: (text) => node("comment", text),
  patchProp: (element, key, _, value) => { element.props[key] = value },
  setText: (element, text) => { element.text = text },
  setElementText: (element, text) => { element.text = text; element.children = [] },
  parentNode: (element) => element.parent,
  nextSibling: (element) => element.parent?.children[element.parent.children.indexOf(element) + 1],
  insert(element, parent, anchor) {
    if (element.parent) element.parent.children.splice(element.parent.children.indexOf(element), 1)
    const index = anchor ? parent.children.indexOf(anchor) : parent.children.length
    parent.children.splice(index, 0, element); element.parent = parent
  },
  remove(element) {
    if (element.parent) element.parent.children.splice(element.parent.children.indexOf(element), 1)
    element.parent = undefined
  },
})
const text = (element) => (element.type === "comment" ? "" : element.text) + element.children.map(text).join("")
const all = (element) => [element, ...element.children.flatMap(all)]
const flush = async () => { for (let i = 0; i < 12; i++) { await Promise.resolve(); await nextTick() } }

const entry = (modelId, metadata, extra = {}) => ({
  model: {
    modelId, displayName: modelId, status: "active",
    capabilities: { modalities: ["text"], toolCalling: true, streaming: true, structuredOutput: true, vision: false, audio: false, reasoning: false },
    metadata,
  },
  source: "default", enabled: true, endpoint: null, unavailableReason: null, missingLimits: [], actions: ["edit"], ...extra,
})

const reply = {
  next: null, drivers: [], errors: [],
  providers: [{
  providerId: "relay", displayName: "Relay", source: "custom", credentialAuthority: "static",
  version: 1, enabled: true, actions: [],
  connection: { driverKind: "openai-compatible", auth: { type: "bearer" }, baseUrl: "https://relay.test/v1", credentialConfigured: true, headerNames: [], proxyUrl: null, proxyAuthConfigured: false, timeoutMs: null },
  preferences: {},
  models: [
    entry("auto-kimi-k3", { identification: { rule: "token", narrowedByDisplayName: false, candidates: [], filled: [] }, matchedProviderId: "moonshot", matchedModelId: "kimi-k3" }),
    entry("gpt-5.5", { identification: { rule: "merge", narrowedByDisplayName: false, candidates: [], filled: [] }, matchedProviderId: "openai", matchedModelId: "gpt-5.5" }),
    entry("ark-code-latest", { identification: { rule: "ambiguous", narrowedByDisplayName: false, candidates: [{ providerId: "ark-code", modelId: "ark-code-latest" }, { providerId: "byteplus-coding-plan", modelId: "ark-code-latest" }], filled: [] } }),
    entry("nothing-names-this", undefined),
  ],
  }],
}
// The provider as the settings page builds it: through the shared reader, so
// a field the reader drops never reaches the list.
const provider = readManagement(reply).providers[0]

test("a model row says how its id was identified, and an ambiguous one names the rows", async (t) => {
  globalThis.modelsConnection = reactive({ phase: "connected", supports: () => true, client: { call: async () => ({}) } })
  const root = node("root"), app = renderer.createApp(ProviderModels, { provider }).use(nuxtUiTestPlugin)
  app.mount(root)
  t.after(() => { app.unmount(); delete globalThis.modelsConnection })
  await flush()
  const screen = text(root)
  assert.match(screen, /identifiedAt kimi-k3 moonshot/, screen)
  assert.match(screen, /identifiedInferred gpt-5\.5 openai/, screen)
  assert.match(screen, /identifiedAmbiguous ark-code\/ark-code-latest, byteplus-coding-plan\/ark-code-latest/, screen)
  assert.match(screen, /identifiedNone/, screen)
})

test("model management searches on the server, loads more and rereads only a changed model", async t => {
  const calls = [], loads = [], updates = []
  const shown = { ...provider, models: [entry("visible", undefined, { actions: ["set_enabled", "edit"] }), entry("other", undefined, { actions: ["delete"] })] }
  globalThis.modelsConnection = reactive({ phase: "connected", supports: () => true, client: { call: async (method, params, read) => {
    calls.push([method, params])
    if (method === "provider.model.set_enabled") return read({ version: 2 })
    if (method === "provider.models.delete") return read({ version: 3 })
    if (method === "provider.management") return read({ drivers: [], errors: [], next: null, providers: [{ ...shown, version: 2, models: [entry("visible", undefined, { enabled: false, actions: ["set_enabled", "edit"] })] }] })
    throw new Error(`unexpected ${method}`)
  } } })
  const root = node("root")
  const app = renderer.createApp(ProviderModels, { provider: shown, next: "other", onLoad: (...args) => loads.push(args), onUpdated: value => updates.push(value) }).use(nuxtUiTestPlugin)
  app.mount(root)
  t.after(() => { app.unmount(); delete globalThis.modelsConnection })
  await flush()
  all(root).find(n => n.props.type === "search").props["onUpdate:modelValue"]("later-page")
  await flush()
  assert.deepEqual(loads[0], [false, "later-page"])
  all(root).find(n => n.type === "button" && text(n) === "loadMore").props.onClick()
  assert.deepEqual(loads[1], [true, "later-page"])
  all(root).find(n => n.props.type === "search").props["onUpdate:modelValue"]("")
  await flush()
  all(root).find(n => n.type === "input" && n.props["aria-label"] === "enabled visible").props["onUpdate:modelValue"](false)
  await flush()
  assert.deepEqual(calls, [
    ["provider.model.set_enabled", { providerId: "relay", expectedVersion: 1, modelId: "visible", enabled: false }],
    ["provider.management", { providerId: "relay", modelId: "visible" }],
  ])
  assert.equal(updates[0].models.length, 2)
  assert.equal(updates[0].models.find(m => m.model.modelId === "visible").enabled, false)
  assert.equal(updates[0].models.find(m => m.model.modelId === "other").enabled, true)
  all(root).find(n => n.type === "button" && text(n) === "remove").props.onClick()
  await flush()
  assert.deepEqual(calls.at(-1), ["provider.models.delete", { providerId: "relay", expectedVersion: 1, modelId: "other", reply: "summary" }])
  all(root).find(n => n.type === "button" && text(n) === "edit").props.onClick()
  await flush()
  assert.deepEqual(calls.at(-1), ["provider.management", { providerId: "relay", modelId: "visible" }])
  assert.ok(all(root).some(n => n.type === "form"))
})
