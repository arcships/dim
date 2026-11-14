import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { registerHooks, stripTypeScriptTypes } from "node:module"
import test from "node:test"
import { compileScript, parse } from "vue/compiler-sfc"
import { createRenderer, nextTick, reactive } from "vue"
import { nuxtUiTestPlugin } from "./helpers/nuxt-ui.mjs"

globalThis.Document = class {}
globalThis.ShadowRoot = class {}
const vue = import.meta.resolve("vue")
registerHooks({
  resolve(specifier, context, next) {
    if (specifier === "../../stores/connection") return { url: "data:text/javascript,export const useConnectionStore=()=>globalThis.shellStores.connection", shortCircuit: true }
    if (specifier === "../../stores/ui") return { url: "data:text/javascript,export const useUiStore=()=>globalThis.shellStores.ui", shortCircuit: true }
    if (specifier === "./context") return { url: "data:text/javascript,export const settingsDirtyKey=Symbol('settings-dirty')", shortCircuit: true }
    if (specifier === "vue-i18n") return { url: "data:text/javascript,export const useI18n=()=>({t:key=>key,locale:{value:'en'}})", shortCircuit: true }
    if (specifier === "vue-router") return {
      url: `data:text/javascript,import{h,defineComponent}from'${vue}';export const useRoute=()=>globalThis.shellRoute;export const useRouter=()=>({replace:async to=>{globalThis.shellNavigations.push(to)},push:async to=>{globalThis.shellNavigations.push(to)}});export const RouterLink={props:['to'],setup:(props,{slots})=>()=>h('a',{to:props.to},slots.default?.())};const Child=defineComponent({render(){return h('div',{class:'child'},'child')}});export const RouterView=defineComponent({setup:(_,{slots})=>()=>slots.default?.({Component:Child})??null});export const onBeforeRouteLeave=()=>{};export const onBeforeRouteUpdate=()=>{}`,
      shortCircuit: true,
    }
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
    return { format: "module", source: stripTypeScriptTypes(compileScript(descriptor, { id: "settings-shell", inlineTemplate: true }).content, { mode: "transform" }), shortCircuit: true }
  },
})
const { default: SettingsPage } = await import("../src/features/settings/SettingsPage.vue")
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

test("the archived category stays usable while the connection is down", async t => {
  globalThis.window = { addEventListener() {}, removeEventListener() {} }
  globalThis.shellRoute = reactive({ name: "settings-archived", path: "/settings/archived" })
  globalThis.shellNavigations = []
  globalThis.shellStores = {
    connection: reactive({ phase: "disconnected", client: { call: async () => ({}) } }),
    ui: reactive({ shellMode: "regular" }),
  }
  const root = node("root"), app = renderer.createApp(SettingsPage).use(nuxtUiTestPlugin)
  app.mount(root)
  t.after(() => { app.unmount(); delete globalThis.window; delete globalThis.shellRoute; delete globalThis.shellStores; delete globalThis.shellNavigations })
  await flush()

  const fieldset = () => all(root).find(element => element.type === "fieldset")
  assert.ok(fieldset())
  assert.ok(!fieldset().props.disabled, "browser-owned archive actions remain available offline")
  assert.match(text(root), /child/)

  globalThis.shellRoute.name = "settings-agent"
  globalThis.shellRoute.path = "/settings/agent"
  await flush()
  assert.ok(fieldset().props.disabled, "daemon-dependent categories stay gated")

  globalThis.shellRoute.name = "settings-appearance"
  globalThis.shellRoute.path = "/settings/appearance"
  await flush()
  assert.ok(!fieldset().props.disabled)
})
