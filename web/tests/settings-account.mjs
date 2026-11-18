import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { registerHooks, stripTypeScriptTypes } from "node:module"
import test from "node:test"
import { compileScript, parse } from "vue/compiler-sfc"
import { createRenderer, defineComponent, nextTick, reactive, ref } from "vue"
import { createPinia } from "pinia"
import { nuxtUiTestPlugin } from "./helpers/nuxt-ui.mjs"

globalThis.Document = class {}
globalThis.ShadowRoot = class {}

const settingsComponent = (url) => /\/(AccountSettings|ProviderAuthFlow|ProviderConnectionForm|ProviderModels|ProviderSettings)\.vue$/.test(url)
registerHooks({
  resolve(specifier, context, next) {
    if (specifier === "../../stores/connection" || (specifier === "./connection" && context.parentURL?.endsWith("/stores/models.ts"))) {
      return { url: "data:text/javascript,export const useConnectionStore=()=>globalThis.settingsConnection", shortCircuit: true }
    }
    if (specifier === "vue-router") return { url: "data:text/javascript,export const useRoute=()=>globalThis.settingsRoute;export const useRouter=()=>globalThis.settingsRouter??{}", shortCircuit: true }
    if (specifier === "vue-i18n") return { url: "data:text/javascript,export const useI18n=()=>({t:(key,args)=>[key,args?.reason??args?.name].filter(Boolean).join(' '),locale:{value:'en'}})", shortCircuit: true }
    if (specifier.endsWith(".vue") && !settingsComponent(specifier)) return { url: "data:text/javascript,export default {setup(props,{expose}){expose({open:async()=>true});return ()=>null}}", shortCircuit: true }
    try { return next(specifier, context) } catch (error) {
      if ((specifier.startsWith("./") || specifier.startsWith("../")) && !/\.(ts|vue)$/.test(specifier)) return next(`${specifier}.ts`, context)
      throw error
    }
  },
  load(url, context, next) {
    if (url.endsWith(".ts")) return { format: "module", source: stripTypeScriptTypes(readFileSync(new URL(url), "utf8"), { mode: "transform" }), shortCircuit: true }
    if (!settingsComponent(url)) return next(url, context)
    const { descriptor } = parse(readFileSync(new URL(url), "utf8"))
    return { format: "module", source: stripTypeScriptTypes(compileScript(descriptor, { id: "settings-account-test", inlineTemplate: true }).content, { mode: "transform" }), shortCircuit: true }
  },
})

const { default: AccountSettings } = await import("../src/features/settings/AccountSettings.vue")
const { default: ProviderSettings } = await import("../src/features/settings/ProviderSettings.vue")
const { useModels } = await import("../src/stores/models.ts")
const { settingsDirtyKey } = await import("../src/features/settings/context.ts")
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

test("an uncertain reset reuses its key after a usage refresh, even when no credits remain", async () => {
  const requests = []
  globalThis.settingsConnection = { phase: "connected", client: { call: async (method, params, read) => {
    if (method === "provider.management" || method === "provider.management.list") return read({ next: null, drivers: [], errors: [], providers: [{
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
  const app = renderer.createApp(AccountSettings).use(createPinia()).use(nuxtUiTestPlugin)
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
    if (method === "provider.management" || method === "provider.management.list") return read({ next: null, drivers: [], errors: [], providers: [{
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
  const app = renderer.createApp(AccountSettings).use(createPinia()).use(nuxtUiTestPlugin)
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

test("Dim device sign-in fetches models and the signed-in account can retry a failed fetch", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] })
  const providerId = "dimcode-api-oauth"
  let signedIn = false
  let refreshed = false
  let refreshes = 0
  globalThis.settingsConnection = { phase: "connected", supports: () => true, client: { call: async (method, params, read) => {
    if (method === "provider.management" || method === "provider.management.list") return read({ next: null, drivers: [], errors: [], providers: [{
      providerId, displayName: "Dim", source: "managed", credentialAuthority: "account", version: null, enabled: true,
      actions: signedIn ? ["refresh"] : ["login"], connection: null, models: null, preferences: {},
      account: { namespace: "account", loginMethods: ["device"], state: { account: signedIn ? { issuer: "https://example.test", subject: "42" } : null } },
    }] })
    if (method === "account.providers") return read([{ providerId, displayName: "Dim", defaultDirectory: "https://dimagent.cn" }])
    if (method === "account.login.begin") return read({ attemptId: "device", expiresAt: Date.now() + 60_000, verificationUri: "https://example.test/device", userCode: "CODE" })
    if (method === "account.login.status") { signedIn = true; return read({ status: "completed" }) }
    if (method === "account.usage") return read({ windows: [], features: [] })
    if (method === "account.referral") return read({ enabled: false, eligible: false, invitedCount: 0, paidCount: 0, inviterRebateBp: 0, inviteeRebateBp: 0 })
    if (method === "provider.list") return read({ providers: [] })
    if (method === "provider.catalog") return read({ catalog: refreshed ? { models: [{ id: "relay-model", name: "Relay model" }], disabledModelIds: [] } : null })
    assert.equal(method, "provider.refresh")
    assert.deepEqual(params, { providerId })
    assert.equal(signedIn, true)
    if (++refreshes === 1) throw new Error("Relay timeout")
    refreshed = true
    return read({})
  } } }
  const root = node("root")
  const pinia = createPinia()
  const app = renderer.createApp(AccountSettings).use(pinia).use(nuxtUiTestPlugin)
  app.mount(root)
  try {
    await flush()
    const form = all(root).find((element) => element.type === "form")
    await form.props.onSubmit({ preventDefault() {} })
    t.mock.timers.tick(100)
    await flush()
    assert.equal(refreshes, 1)
    assert.match(text(root), /signOut/)
    assert.match(text(root), /modelRefreshFailed Relay timeout/)
    assert.equal(all(root).some((element) => element.type === "form"), false)
    const refresh = all(root).find((element) => element.type === "button" && text(element) === "refreshModelList")
    await refresh.props.onClick()
    await flush()
    assert.equal(refreshes, 2)
    assert.match(text(root), /modelListRefreshed/)
    assert.doesNotMatch(text(root), /Relay timeout/)
    assert.equal(useModels().providers, undefined)
  } finally { app.unmount(); delete globalThis.settingsConnection }
})

test("disabled third-party Provider refresh retries and loads model detail only on its tab", async () => {
  const providerId = "relay"
  let refreshes = 0
  const modelReads = []
  const provider = { providerId, displayName: "Relay", source: "custom", credentialAuthority: "static", version: 1, enabled: false, actions: ["refresh"], preferences: {},
    connection: { driverKind: "openai-compatible", auth: { type: "bearer" }, baseUrl: "https://example.test/v1", credentialConfigured: true, headerNames: [], proxyUrl: null, proxyAuthConfigured: false, timeoutMs: null },
  }
  globalThis.settingsRoute = { name: "settings-provider-detail", params: { providerId } }
  globalThis.settingsConnection = { phase: "connected", supports: () => false, client: { call: async (method, params, read) => {
    if (method === "provider.management") modelReads.push(params)
    if (method === "provider.management" || method === "provider.management.list") return read({ next: null, drivers: [], errors: [], providers: [{
      ...provider, models: refreshes < 2 ? [] : [{
        model: { modelId: "relay-model", displayName: "Relay model", status: "active", capabilities: {
          modalities: ["text"], toolCalling: true, streaming: true, structuredOutput: true, vision: false, audio: false, reasoning: false,
        } },
        source: "default", enabled: true, endpoint: null, unavailableReason: null, actions: [],
      }],
    }] })
    if (method === "provider.list") return read({ providers: [{ ...provider, credentialConfigured: true }] })
    if (method === "provider.catalog") return read({ catalog: { models: [], disabledModelIds: [] } })
    assert.equal(method, "provider.refresh")
    assert.deepEqual(params, { providerId })
    if (++refreshes === 1) throw new Error("Relay timeout")
    return read({})
  } } }
  const root = node("root")
  const app = renderer.createApp(ProviderSettings).use(createPinia()).use(nuxtUiTestPlugin).provide(settingsDirtyKey, ref(false))
  app.mount(root)
  try {
    await flush()
    const button = (label) => all(root).find((element) => element.type === "button" && text(element) === label)
    await button("refreshModelList").props.onClick()
    await flush()
    assert.match(text(root), /modelRefreshFailed Relay timeout/)
    await button("retry").props.onClick()
    await flush()
    assert.equal(refreshes, 2)
    assert.doesNotMatch(text(root), /Relay timeout/)
    assert.deepEqual(modelReads, [])
    const modelsTab = all(root).find((element) => element.props.role === "tab" && text(element) === "models")
    await modelsTab.props.onClick(); await flush()
    assert.deepEqual(modelReads, [{ providerId, page: { query: "", limit: 32 } }])
  } finally { app.unmount(); delete globalThis.settingsConnection; delete globalThis.settingsRoute }
})

test("a new Provider states the prefix its ids carry, and an empty field states none", async () => {
  const drivers = [{ driverKind: "openai-compatible", authTypes: ["bearer"], remoteModelList: true }]
  const createDraft = async (prefix) => {
    const drafts = []
    globalThis.settingsRoute = { name: "settings-provider-new", params: {} }
    globalThis.settingsRouter = { replace: async () => {} }
    globalThis.settingsConnection = { phase: "connected", supports: () => false, client: { call: async (method, params, read) => {
      if (method === "provider.management" || method === "provider.management.list") return read({ next: null, drivers, errors: [], providers: [] })
      if (method === "provider.list") return read({ providers: [] })
      if (method === "provider.probe") return read({ models: [] })
      assert.equal(method, "provider.create")
      drafts.push(params)
      return read({
        providerId: params.providerId, displayName: params.displayName, source: "custom", credentialAuthority: "static",
        version: 1, enabled: true, actions: [], models: [], preferences: {},
        connection: { ...params.connection, credentialConfigured: false, headerNames: [], proxyUrl: null, proxyAuthConfigured: false, timeoutMs: null, ...(params.gluedPrefix ? { gluedPrefix: params.gluedPrefix } : {}) },
      })
    } } }
    const root = node("root")
    // The prefix lives among the advanced fields, so the test renders their
    // content as an expanded section would.
    const expanded = defineComponent({ setup(_, { slots }) { return () => [slots.default?.({ open: true }), slots.content?.()] } })
    const app = renderer.createApp(ProviderSettings).use(createPinia()).use(nuxtUiTestPlugin, { UCollapsible: expanded }).provide(settingsDirtyKey, ref(false))
    app.mount(root)
    try {
      await flush()
      await all(root).find((element) => element.type === "button" && text(element) === "addCustomProvider").props.onClick()
      await flush()
      const field = (label) => all(root).find((element) => element.type === "label" && text(element).startsWith(label)).children.find((element) => element.type === "input")
      field("providerName").props["onUpdate:modelValue"]("My relay")
      field("baseUrl").props["onUpdate:modelValue"]("https://example.test/v1")
      field("modelIdPrefix").props["onUpdate:modelValue"](prefix)
      await flush()
      await all(root).find((element) => element.type === "form").props.onSubmit({ preventDefault() {} })
      await flush()
      assert.equal(drafts.length, 1)
      return drafts[0]
    } finally { app.unmount(); delete globalThis.settingsConnection; delete globalThis.settingsRoute; delete globalThis.settingsRouter }
  }
  const stated = await createDraft("auto")
  assert.equal(stated.gluedPrefix, "auto")
  const blank = await createDraft("")
  assert.equal(Object.hasOwn(blank, "gluedPrefix"), false)
  assert.deepEqual(Object.keys(blank).sort(), ["connection", "displayName", "enabled", "models", "providerId"])
})

test("a Provider save keeps the draft on failure and closes the dialog on success", async () => {
  const route = reactive({ name: "settings-provider-new", params: {} })
  globalThis.settingsRoute = route
  globalThis.settingsRouter = { replace: async ({ name }) => { route.name = name } }
  const drafts = []
  const drivers = [{ driverKind: "openai-compatible", authTypes: ["bearer"], remoteModelList: true }]
  globalThis.settingsConnection = { phase: "connected", supports: () => false, client: { call: async (method, params, read) => {
    if (method === "provider.management" || method === "provider.management.list") return read({ next: null, drivers, errors: [], providers: [] })
    if (method === "provider.list") return read({ providers: [] })
    if (method === "provider.probe") return read({ models: [] })
    assert.equal(method, "provider.create")
    drafts.push(params)
    if (drafts.length === 1) throw new Error("Provider store unavailable")
    return read({
      providerId: params.providerId, displayName: params.displayName, source: "custom", credentialAuthority: "static",
      version: 1, enabled: false, actions: ["models.create"], models: [], preferences: {},
      connection: { ...params.connection, credentialConfigured: false, headerNames: [], proxyUrl: null, proxyAuthConfigured: false, timeoutMs: null },
    })
  } } }
  const root = node("root")
  const dirty = ref(false)
  const app = renderer.createApp(ProviderSettings).use(createPinia()).use(nuxtUiTestPlugin).provide(settingsDirtyKey, dirty)
  app.mount(root)
  try {
    await flush()
    const button = (label) => all(root).find((element) => element.type === "button" && text(element) === label)
    const field = (label) => all(root).find((element) => element.type === "label" && text(element) === label).children.find((element) => element.type === "input")
    await button("addCustomProvider").props.onClick()
    await flush()
    const dialog = () => all(root).find((element) => element.props.role === "dialog")
    assert.ok(dialog())
    field("providerName").props["onUpdate:modelValue"]("My relay")
    field("baseUrl").props["onUpdate:modelValue"]("https://example.test/v1")
    field("enableProvider").props["onUpdate:modelValue"](false)
    await flush()
    const form = all(root).find((element) => element.type === "form")
    const submit = () => form.props.onSubmit({ preventDefault() {} })
    assert.equal(dirty.value, true)
    await submit()
    await flush()
    assert.ok(dialog())
    assert.match(text(root), /Provider store unavailable/)
    assert.equal(dirty.value, true)
    await submit()
    await flush()
    assert.deepEqual(drafts[1], drafts[0])
    assert.equal(route.name, "settings-providers")
    assert.equal(dialog(), undefined)
    assert.equal(dirty.value, false)
    assert.match(text(root), /providerSaved My relay/)
  } finally { app.unmount(); delete globalThis.settingsConnection; delete globalThis.settingsRoute; delete globalThis.settingsRouter }
})

test("provider overview follows pages on demand and opens only the selected detail", async () => {
  const requests = []
  const info = (id) => ({ providerId: id, displayName: id, source: "custom", credentialAuthority: "static", enabled: true, version: 1, actions: [], account: undefined, connection: null })
  globalThis.settingsRoute = reactive({ name: "settings-providers", params: {} })
  globalThis.settingsRouter = { push: async (route) => { Object.assign(globalThis.settingsRoute, { ...route, params: route.params ?? {} }) } }
  globalThis.settingsConnection = { phase: "connected", client: { call: async (method, params, read) => {
    requests.push({ method, params })
    if (method === "provider.management.list") return read({ drivers: [], errors: [], providers: [info(params.providerId ?? (params.after ? "second" : "first"))], next: params.providerId || params.after ? null : "first" })
    assert.equal(method, "provider.management")
    assert.deepEqual(params, { providerId: "second", page: { query: "", limit: 32 } })
    return read({ next: null, drivers: [], errors: [], providers: [{ ...info("second"), models: [], preferences: {} }] })
  } } }
  const root = node("root")
  const app = renderer.createApp(ProviderSettings).use(createPinia()).use(nuxtUiTestPlugin).provide(settingsDirtyKey, ref(false))
  app.mount(root)
  try {
    await flush()
    assert.equal(requests.length, 1, "opening the list fetches one overview page")
    assert.doesNotMatch(text(root), /second/)
    const more = all(root).find((element) => element.type === "button" && text(element) === "loadMore")
    await more.props.onClick(); await flush()
    assert.equal(requests[1].params.after, "first")
    const second = all(root).find((element) => element.type === "button" && text(element).includes("second"))
    await second.props.onClick(); await flush()
    assert.equal(requests.filter(({ method }) => method === "provider.management").length, 0)
    assert.deepEqual(requests.at(-1), { method: "provider.management.list", params: { providerId: "second" } })
    const modelsTab = all(root).find((element) => element.props.role === "tab" && text(element) === "models")
    await modelsTab.props.onClick(); await flush()
    assert.equal(requests.filter(({ method }) => method === "provider.management").length, 1)
    assert.deepEqual(requests.at(-1).params, { providerId: "second", page: { query: "", limit: 32 } })
    assert.doesNotMatch(text(root), /AssertionError/)
    assert.ok(all(root).some((element) => element.props.class === "provider-models"))
  } finally {
    app.unmount()
    delete globalThis.settingsRoute
    delete globalThis.settingsRouter
    delete globalThis.settingsConnection
  }
})

test("connection configuration reads and writes summaries without requesting model payloads", async () => {
  const requests = []
  const route = reactive({ name: "settings-provider-detail", params: { providerId: "fixture" } })
  const provider = { providerId: "fixture", displayName: "Fixture", source: "builtin", credentialAuthority: "static", enabled: false, version: 2, actions: ["update"],
    connection: { driverKind: "openai-compatible", auth: { type: "bearer" }, baseUrl: "https://example.test/v1", credentialConfigured: true, headerNames: [], proxyUrl: null, proxyAuthConfigured: false, timeoutMs: null } }
  globalThis.settingsRoute = route
  globalThis.settingsRouter = { replace: async ({ name }) => { route.name = name; route.params = {} } }
  globalThis.settingsConnection = { phase: "connected", client: { call: async (method, params, read) => {
    requests.push({ method, params })
    if (method === "provider.management.list") return read({ drivers: [], providers: [provider], errors: [], next: null })
    assert.equal(method, "provider.update")
    assert.equal(params.reply, "summary")
    assert.equal(params.expectedVersion, 2)
    assert.deepEqual(params.patch, { enabled: true })
    return read({ ...provider, version: 3, enabled: true })
  } } }
  const root = node("root")
  const app = renderer.createApp(ProviderSettings).use(createPinia()).use(nuxtUiTestPlugin).provide(settingsDirtyKey, ref(false))
  app.mount(root)
  try {
    await flush()
    const label = all(root).find((element) => element.type === "label" && text(element) === "enableProvider")
    label.children.find((element) => element.type === "input").props["onUpdate:modelValue"](true)
    await flush()
    await all(root).find((element) => element.type === "form").props.onSubmit({ preventDefault() {} })
    await flush()
    assert.equal(requests.filter(({ method }) => method === "provider.update").length, 1)
    assert.equal(route.name, "settings-providers")
  } finally { app.unmount(); delete globalThis.settingsRoute; delete globalThis.settingsRouter; delete globalThis.settingsConnection }
})

test("model edits and creation keep the owner's active search results", async () => {
  const requests = []
  const provider = { providerId: "relay", displayName: "Relay", source: "custom", credentialAuthority: "static", enabled: true, version: 1, actions: ["models.create"], connection: null }
  const entries = [{ model: { modelId: "first", displayName: "Needle", status: "active",
    capabilities: { modalities: ["text"], toolCalling: true, streaming: true, structuredOutput: true, vision: false, audio: false, reasoning: false } },
    source: "user", enabled: true, endpoint: null, unavailableReason: null, missingLimits: [], actions: ["edit"] }]
  globalThis.settingsRoute = reactive({ name: "settings-provider-detail", params: { providerId: "relay" } })
  globalThis.settingsConnection = { phase: "connected", client: { call: async (method, params, read) => {
    requests.push({ method, params })
    if (method === "provider.management.list") return read({ drivers: [], providers: [provider], errors: [], next: null })
    if (method === "provider.models.edit" || method === "provider.models.create") {
      assert.equal(params.reply, "summary")
      if (method === "provider.models.edit") entries[0].model.displayName = params.edit.displayName
      else entries.push({ ...entries[0], model: params.model })
      provider.version++
      return read({ version: provider.version })
    }
    assert.equal(method, "provider.management")
    const query = (params.page?.query ?? "").toLowerCase()
    const models = entries.filter(({ model }) => params.modelId ? model.modelId === params.modelId
      : model.modelId.toLowerCase().includes(query) || model.displayName.toLowerCase().includes(query))
    return read({ drivers: [], providers: [{ ...provider, models, preferences: {} }], errors: [], next: null })
  } } }
  const root = node("root")
  const app = renderer.createApp(ProviderSettings).use(createPinia()).use(nuxtUiTestPlugin).provide(settingsDirtyKey, ref(false))
  app.mount(root)
  try {
    await flush()
    await all(root).find(element => element.props.role === "tab" && text(element) === "models").props.onClick()
    await flush()
    all(root).filter(element => element.props.type === "search").at(-1).props["onUpdate:modelValue"]("Needle")
    await flush()
    await all(root).find(element => element.type === "button" && text(element) === "edit").props.onClick()
    await flush()
    const fields = () => all(root).filter(element => element.type === "input" && element.props.required)
    fields()[1].props["onUpdate:modelValue"]("Renamed")
    await all(root).find(element => element.type === "form").props.onSubmit({ preventDefault() {} })
    await flush()
    const modelNames = () => all(root).filter(element => element.props.class === "provider-models")
      .flatMap(element => all(element).filter(row => row.type === "strong").map(text))
    assert.deepEqual(requests.at(-1), { method: "provider.management", params: { providerId: "relay", page: { query: "Needle", limit: 32 } } })
    assert.deepEqual(modelNames(), [])
    await all(root).find(element => element.type === "button" && text(element) === "addModel").props.onClick()
    await flush()
    fields()[0].props["onUpdate:modelValue"]("second")
    fields()[1].props["onUpdate:modelValue"]("Unrelated")
    await all(root).find(element => element.type === "form").props.onSubmit({ preventDefault() {} })
    await flush()
    assert.equal(requests.at(-2).method, "provider.models.create")
    assert.deepEqual(requests.at(-1).params.page, { query: "Needle", limit: 32 })
    assert.deepEqual(modelNames(), [])
    all(root).filter(element => element.props.type === "search").at(-1).props["onUpdate:modelValue"]("")
    await flush()
    assert.deepEqual(modelNames(), ["Renamed", "Unrelated"])
  } finally { app.unmount(); delete globalThis.settingsRoute; delete globalThis.settingsConnection }
})

test("a model search replaces a previous page cursor before load more can extend it", async () => {
  const requests = []
  const pendingSearch = Promise.withResolvers()
  const route = reactive({ name: "settings-provider-detail", params: { providerId: "relay" } })
  const provider = { providerId: "relay", displayName: "Relay", source: "custom", credentialAuthority: "static", enabled: true, version: 1, actions: [],
    connection: { driverKind: "openai-compatible", auth: { type: "bearer" }, baseUrl: "https://example.test/v1", credentialConfigured: true, headerNames: [], proxyUrl: null, proxyAuthConfigured: false, timeoutMs: null } }
  const entry = (modelId) => ({ model: { modelId, displayName: modelId, status: "active",
    capabilities: { modalities: ["text"], toolCalling: true, streaming: true, structuredOutput: false, vision: false, audio: false, reasoning: false }, metadata: undefined },
    source: "default", enabled: true, endpoint: null, unavailableReason: null, missingLimits: [], actions: [] })
  globalThis.settingsRoute = route
  globalThis.settingsRouter = {}
  globalThis.settingsConnection = { phase: "connected", client: { call: async (method, params, read) => {
    requests.push({ method, params })
    if (method === "provider.management.list") return read({ drivers: [], providers: [provider], errors: [], next: null })
    assert.equal(method, "provider.management")
    const query = params.page?.query ?? ""
    if (query === "delayed" && !params.page?.after) await pendingSearch.promise
    return read({ drivers: [], errors: [], next: query ? "search-cursor" : "cursor-1",
      providers: [{ ...provider, preferences: {}, models: [entry(params.page?.after ? `${query}-next` : query || "first")] }] })
  } } }
  const root = node("root")
  const app = renderer.createApp(ProviderSettings).use(createPinia()).use(nuxtUiTestPlugin).provide(settingsDirtyKey, ref(false))
  app.mount(root)
  try {
    await flush()
    await all(root).find((element) => element.props.role === "tab" && text(element) === "models").props.onClick()
    await flush()
    const before = requests.filter(({ method }) => method === "provider.management")
    assert.equal(before.length, 1)
    assert.equal(before[0].params.page.after, undefined)
    const search = all(root).filter((element) => element.type === "input" && element.props.type === "search").at(-1)
    search.props["onUpdate:modelValue"]("later")
    await all(root).find((element) => element.type === "button" && text(element) === "loadMore").props.onClick()
    await flush()
    const searched = requests.filter(({ method, params }) => method === "provider.management" && params.page?.query === "later")
    assert.ok(searched.length > 0)
    assert.ok(searched.every(({ params }) => params.page.after === undefined), JSON.stringify(searched))
    await all(root).find((element) => element.type === "button" && text(element) === "loadMore").props.onClick()
    await flush()
    assert.deepEqual(requests.at(-1).params.page, { query: "later", limit: 32, after: "search-cursor" })
    const modelNames = () => all(root).filter((element) => element.type === "div" && element.props.class === "provider-models")
      .flatMap((element) => all(element).filter((row) => row.type === "strong").map(text))
    assert.deepEqual(modelNames(), ["later", "later-next"])
    const queuedLoadMore = all(root).find((element) => element.type === "button" && text(element) === "loadMore").props.onClick
    search.props["onUpdate:modelValue"]("delayed")
    await flush()
    await queuedLoadMore()
    await flush()
    const delayed = requests.filter(({ method, params }) => method === "provider.management" && params.page?.query === "delayed")
    assert.equal(delayed.length, 1, "a pending first page owns the query until it supplies a continuation")
    assert.deepEqual(delayed[0].params.page, { query: "delayed", limit: 32 })
    pendingSearch.resolve()
    await flush()
    assert.deepEqual(modelNames(), ["delayed"], "the first page replaces every row from the previous query")
    await all(root).find((element) => element.type === "button" && text(element) === "loadMore").props.onClick()
    await flush()
    assert.deepEqual(requests.at(-1).params.page, { query: "delayed", limit: 32, after: "search-cursor" })
    assert.deepEqual(modelNames(), ["delayed", "delayed-next"])
    await all(root).find((element) => element.props.role === "tab" && text(element) === "connection").props.onClick()
    await flush()
    await all(root).find((element) => element.props.role === "tab" && text(element) === "models").props.onClick()
    await flush()
    assert.deepEqual(requests.at(-1).params.page, { query: "", limit: 32 })
    assert.deepEqual(modelNames(), ["first"])
    const reopenedSearch = all(root).filter((element) => element.type === "input" && element.props.type === "search").at(-1)
    assert.equal(reopenedSearch.props.value, "")
  } finally { app.unmount(); delete globalThis.settingsRoute; delete globalThis.settingsRouter; delete globalThis.settingsConnection }
})
