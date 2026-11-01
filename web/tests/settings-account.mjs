import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { registerHooks, stripTypeScriptTypes } from "node:module"
import test from "node:test"
import { compileScript, parse } from "vue/compiler-sfc"
import { createRenderer, nextTick, reactive } from "vue"

registerHooks({
  resolve(specifier, context, next) {
    if (context.parentURL?.endsWith("AccountSettings.vue")) {
      if (specifier === "../../stores/connection") return { url: "data:text/javascript,export const useConnectionStore=()=>globalThis.settingsConnection", shortCircuit: true }
      if (specifier === "../../stores/models") return { url: "data:text/javascript,export const useModelsStore=()=>({load:async()=>{}})", shortCircuit: true }
    }
    if (specifier === "vue-i18n") return { url: "data:text/javascript,export const useI18n=()=>({t:key=>key,locale:{value:'en'}})", shortCircuit: true }
    if (specifier.endsWith(".vue") && !specifier.endsWith("AccountSettings.vue")) return { url: "data:text/javascript,export default {setup(props,{expose}){expose({open:async()=>true});return ()=>null}}", shortCircuit: true }
    try { return next(specifier, context) } catch (error) {
      if ((specifier.startsWith("./") || specifier.startsWith("../")) && !/\.(ts|vue)$/.test(specifier)) return next(`${specifier}.ts`, context)
      throw error
    }
  },
  load(url, context, next) {
    if (url.endsWith(".ts")) return { format: "module", source: stripTypeScriptTypes(readFileSync(new URL(url), "utf8"), { mode: "transform" }), shortCircuit: true }
    if (!url.endsWith("AccountSettings.vue")) return next(url, context)
    const { descriptor } = parse(readFileSync(new URL(url), "utf8"))
    return { format: "module", source: stripTypeScriptTypes(compileScript(descriptor, { id: "settings-account-test", inlineTemplate: true }).content, { mode: "transform" }), shortCircuit: true }
  },
})

const { default: AccountSettings } = await import("../src/features/settings/AccountSettings.vue")
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

test("an uncertain reset reuses its key after a usage refresh, even when no credits remain", async () => {
  const requests = []
  globalThis.settingsConnection = { phase: "connected", client: { call: async (method, params, read) => {
    if (method === "provider.management") return read({ drivers: [], errors: [], providers: [{
      providerId: "dimcode-api-oauth", displayName: "Dim", source: "managed", credentialAuthority: "account", version: null, enabled: true, actions: [], connection: null, models: null, preferences: {},
      account: { namespace: "account", loginMethods: ["browser"], state: { account: { issuer: "https://example.test", subject: "42" } } },
    }] })
    if (method === "account.usage") return read({ subscriptionId: 7, resets: { window: { availableCount: requests.length ? 0 : 1 }, monthlyFull: { availableCount: 2 } }, windows: [], features: [] })
    if (method === "account.referral") return read({ enabled: false, eligible: false, invitedCount: 0, paidCount: 0, inviterRebateBp: 0, inviteeRebateBp: 0 })
    assert.equal(method, "account.apply_reset")
    requests.push(JSON.parse(JSON.stringify(params)))
    if (requests.length === 1) throw new Error("Reply lost after commit")
    return read({})
  } } }
  const root = node("root")
  const app = renderer.createApp(AccountSettings)
  app.mount(root)
  try {
    await flush()
    const resetButtons = () => all(root).filter((element) => element.type === "button" && text(element) === "reset")
    await resetButtons()[0].props.onClick(); await flush()
    assert.equal(requests.length, 1)
    assert.match(requests[0].idempotencyKey, /^[0-9a-f-]{36}$/)
    assert.equal(resetButtons()[1].props.disabled, true)
    const refresh = all(root).find((element) => element.type === "button" && element.props["aria-label"] === "refresh")
    await refresh.props.onClick(); await flush()
    assert.equal(resetButtons()[0].props.disabled, false)
    await resetButtons()[0].props.onClick(); await flush()
    assert.equal(requests.length, 2)
    assert.deepEqual(requests[1], requests[0])
    assert.equal(resetButtons()[0].props.disabled, true)
    assert.equal(resetButtons()[1].props.disabled, false)
  } finally { app.unmount(); delete globalThis.settingsConnection }
})

test("account refresh retains usage on failure and reconnect never exposes the previous identity's usage", async () => {
  let subject = "first"
  let holdUsage = false
  let resolveUsage, rejectUsage
  globalThis.settingsConnection = reactive({ phase: "connected", client: { call: async (method, params, read) => {
    if (method === "provider.management") return read({ drivers: [], errors: [], providers: [{
      providerId: "dimcode-api-oauth", displayName: "Dim", source: "managed", credentialAuthority: "account", version: null, enabled: true, actions: [], connection: null, models: null, preferences: {},
      account: { namespace: "account", loginMethods: ["browser"], state: { account: { issuer: "https://example.test", subject } } },
    }] })
    if (method === "account.usage") {
      if (holdUsage) return new Promise((resolve, reject) => { resolveUsage = (value) => resolve(read(value)); rejectUsage = reject })
      return read({ plan: { name: "First account plan" }, windows: [], features: [] })
    }
    assert.equal(method, "account.referral")
    return read({ enabled: false, eligible: false, invitedCount: 0, paidCount: 0, inviterRebateBp: 0, inviteeRebateBp: 0 })
  } } })
  const root = node("root")
  const app = renderer.createApp(AccountSettings)
  app.mount(root)
  try {
    await flush()
    const refresh = () => all(root).find((element) => element.type === "button" && element.props["aria-label"] === "refresh").props.onClick()
    assert.match(text(root), /First account plan/)
    holdUsage = true
    const refreshing = refresh()
    await flush()
    assert.match(text(root), /First account plan/)
    rejectUsage(new Error("Usage temporarily unavailable"))
    await refreshing; await flush()
    assert.match(text(root), /Usage temporarily unavailable/)
    assert.match(text(root), /First account plan/)
    globalThis.settingsConnection.phase = "reconnecting"
    await flush()
    assert.equal(all(root).find((element) => element.type === "button" && element.props["aria-label"] === "refresh").props.disabled, true)
    assert.match(text(root), /First account plan/)
    subject = "second"
    globalThis.settingsConnection.phase = "connected"
    await flush()
    assert.doesNotMatch(text(root), /First account plan/)
    resolveUsage({ plan: { name: "Second account plan" }, windows: [], features: [] })
    await flush()
    assert.match(text(root), /Second account plan/)
    assert.doesNotMatch(text(root), /First account plan/)
  } finally { app.unmount(); delete globalThis.settingsConnection }
})
