import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { registerHooks, stripTypeScriptTypes } from "node:module"
import test from "node:test"
import { compileScript, parse } from "vue/compiler-sfc"
import { createRenderer, nextTick, reactive } from "vue"

registerHooks({
  resolve(specifier, context, next) {
    if (context.parentURL?.endsWith("ModalitySettings.vue")) {
      if (specifier === "../../stores/connection") return { url: "data:text/javascript,export const useConnectionStore=()=>globalThis.settingsConnection", shortCircuit: true }
      if (specifier === "../../stores/models") return { url: "data:text/javascript,export const useModelsStore=()=>globalThis.settingsModels", shortCircuit: true }
    }
    if (specifier === "vue-i18n") return { url: "data:text/javascript,export const useI18n=()=>({t:key=>key,locale:{value:'en'}})", shortCircuit: true }
    if (specifier.endsWith(".vue") && !specifier.endsWith("ModalitySettings.vue")) return { url: "data:text/javascript,export default { props: ['busy', 'error', 'saved'], setup() { return () => null } }", shortCircuit: true }
    try { return next(specifier, context) } catch (error) {
      if ((specifier.startsWith("./") || specifier.startsWith("../")) && !/\.(ts|vue)$/.test(specifier)) return next(`${specifier}.ts`, context)
      throw error
    }
  },
  load(url, context, next) {
    if (url.endsWith(".ts")) return { format: "module", source: stripTypeScriptTypes(readFileSync(new URL(url), "utf8"), { mode: "transform" }), shortCircuit: true }
    if (!url.endsWith("ModalitySettings.vue")) return next(url, context)
    const { descriptor } = parse(readFileSync(new URL(url), "utf8"))
    return { format: "module", source: stripTypeScriptTypes(compileScript(descriptor, { id: "settings-modality-test", inlineTemplate: true }).content, { mode: "transform" }), shortCircuit: true }
  },
})

const { default: ModalitySettings } = await import("../src/features/settings/ModalitySettings.vue")
const { RpcError } = await import("../src/rpc/client.ts")
const node = (type, text = "") => ({ type, text, children: [], props: {}, parent: undefined })
const renderer = createRenderer({
  createElement: node, createText: (text) => node("text", text), createComment: (text) => node("comment", text),
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
const PROVIDER = "fixture"
const option = (modelId, available = true, reason) => ({ providerId: PROVIDER, modelId, available, ...(reason === undefined ? {} : { reason }) })
const effective = (modelId, source = "automatic") => ({ providerId: PROVIDER, modelId, source })
const operation = (operation, key, extra) => ({ operation, key, available: true, reasons: [], default: null, effective: null, options: [], ...extra })
function snapshot(revision, operations) {
  return {
    revision,
    categories: [
      { category: "image", available: true, reasons: [], operations },
      { category: "video", available: false, reasons: [{ dimension: "build", detail: "Not in this build" }], operations: [] },
    ],
  }
}
function models() {
  return {
    providers: [{ providerId: PROVIDER, name: "Fixture", models: [{ id: "alpha", name: "Alpha" }, { id: "beta", name: "Beta" }] }],
    error: "",
    load: async () => {},
  }
}
function mount(call) {
  globalThis.settingsModels = models()
  globalThis.settingsConnection = reactive({ phase: "connected", client: { call }, supports: (method) => method === "modality.capabilities" || method === "modality.configure" })
  const root = node("root")
  const app = renderer.createApp(ModalitySettings)
  app.mount(root)
  return { root, app }
}
const selects = (root) => all(root).filter((element) => element.type === "select")
const save = (root) => all(root).find((element) => element.type === "button" && text(element) === "Save defaults")
const submit = (root) => all(root).find((element) => element.type === "form" && element.props.onSubmit !== undefined)

test("modality rows show the automatic effective model and an unavailable default's reason", async () => {
  const { root, app } = mount(async (method, params, read) => {
    assert.equal(method, "modality.capabilities")
    assert.deepEqual(params, {})
    return read(snapshot("native:user/modality:3", [
      operation("generate", "modality.image.generate", { effective: effective("alpha"), options: [option("alpha"), option("beta")] }),
      operation("edit", "modality.image.edit", { available: false, reasons: [{ dimension: "adapter", detail: "No image adapter in this build" }] }),
      operation("enhance", "modality.image.enhance", { available: false, reasons: [{ dimension: "adapter", detail: "No image adapter in this build" }] }),
      operation("understand", "modality.image.understand", { default: option("alpha", false, { dimension: "provider", detail: "Model is no longer selectable" }), options: [option("alpha", false), option("beta")] }),
    ]))
  })
  try {
    await flush()
    const body = text(root)
    assert.match(body, /Image generation/)
    assert.match(body, /Fixture \/ Alpha \(automatic\)/)
    assert.match(body, /No image adapter in this build/)
    assert.match(body, /Currently effective: None\. The configured default is unavailable and is never replaced by another model\./)
    assert.match(body, /Currently effective: None\. No model serves this operation now\./)
    assert.match(body, /Model is no longer selectable/)
    assert.match(body, /the Session image-understanding path does not read this default yet/)
    const [generate, edit, , understand] = selects(root)
    assert.equal(generate.props.value, "")
    assert.doesNotMatch(text(generate), /\(unavailable\)/)
    assert.match(text(understand), /Fixture \/ Alpha \(unavailable\)/)
    assert.equal(understand.props.value, JSON.stringify([PROVIDER, "alpha"]))
    assert.equal(selects(root)[1].props.disabled, false)
    assert.equal(save(root).props.disabled, true, "nothing is dirty yet")
    assert.equal(edit.props.value, "")
  } finally { app.unmount(); delete globalThis.settingsConnection; delete globalThis.settingsModels }
})

test("a conflicting configure keeps the draft and asks for a reload", async () => {
  const calls = []
  const initial = snapshot("native:user/modality:3", [
    operation("generate", "modality.image.generate", { effective: effective("beta"), options: [option("alpha"), option("beta")] }),
    operation("edit", "modality.image.edit", { default: option("beta"), effective: effective("beta", "default"), options: [option("alpha"), option("beta")] }),
    operation("enhance", "modality.image.enhance", { available: false, reasons: [{ dimension: "adapter", detail: "No image adapter in this build" }] }),
    operation("understand", "modality.image.understand", { options: [option("alpha")] }),
  ])
  const { root, app } = mount(async (method, params, read) => {
    if (method === "modality.capabilities") return read(initial)
    assert.equal(method, "modality.configure")
    calls.push(JSON.parse(JSON.stringify(params)))
    if (calls.length === 1) return read(snapshot("native:user/modality:4", [
      operation("generate", "modality.image.generate", { default: option("alpha"), effective: effective("alpha", "default"), options: [option("alpha"), option("beta")] }),
      operation("edit", "modality.image.edit", { default: option("beta"), effective: effective("beta", "default"), options: [option("alpha"), option("beta")] }),
      operation("enhance", "modality.image.enhance", { available: false, reasons: [{ dimension: "adapter", detail: "No image adapter in this build" }] }),
      operation("understand", "modality.image.understand", { options: [option("alpha")] }),
    ]))
    throw new RpcError(-32000, "Modality defaults changed", "modality_conflict")
  })
  try {
    await flush()
    const [generate, edit] = selects(root)
    generate.props.onChange({ target: { value: JSON.stringify([PROVIDER, "alpha"]) } })
    edit.props.onChange({ target: { value: "" } })
    await flush()
    assert.equal(save(root).props.disabled, false)
    submit(root).props.onSubmit({ preventDefault() {} })
    await flush()
    assert.equal(calls.length, 2)
    assert.deepEqual(calls[0], { expectedRevision: "native:user/modality:3", category: "image", operation: "generate", model: { providerId: PROVIDER, modelId: "alpha" } })
    assert.deepEqual(calls[1], { expectedRevision: "native:user/modality:4", category: "image", operation: "edit", model: null })
    assert.equal(selects(root)[0].props.value, JSON.stringify([PROVIDER, "alpha"]), "the saved key keeps its value")
    assert.equal(selects(root)[1].props.value, "", "the conflicting draft is preserved")
    assert.equal(selects(root)[1].props.disabled, true, "a conflict disables further picks until reload")
    assert.equal(save(root).props.disabled, true)
  } finally { app.unmount(); delete globalThis.settingsConnection; delete globalThis.settingsModels }
})
