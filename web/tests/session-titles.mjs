import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { registerHooks, stripTypeScriptTypes } from "node:module"
import test from "node:test"
import { createPinia, setActivePinia } from "pinia"
import { compileScript, parse } from "vue/compiler-sfc"
import { createRenderer, markRaw, nextTick } from "vue"

registerHooks({
  resolve(specifier, context, next) {
    if (specifier === "../i18n") {
      return next(new URL("../src/i18n/index.ts", import.meta.url).href, context)
    }
    if (context.parentURL?.endsWith("SidebarWorkspaceGroup.vue")) {
      if (specifier === "vue-i18n") return { url: "data:text/javascript,export const useI18n=()=>({t:key=>key})", shortCircuit: true }
      if (specifier === "vue-router") return { url: "data:text/javascript,export const RouterLink={template:'<a><slot /></a>'}", shortCircuit: true }
    }
    try { return next(specifier, context) } catch (error) {
      if (specifier.startsWith(".") && !/\.(ts|vue)$/.test(specifier)) return next(`${specifier}.ts`, context)
      throw error
    }
  },
  load(url, context, next) {
    if (url.endsWith(".ts")) return { format: "module", source: stripTypeScriptTypes(readFileSync(new URL(url), "utf8"), { mode: "transform" }), shortCircuit: true }
    if (!url.endsWith("SidebarWorkspaceGroup.vue")) return next(url, context)
    const { descriptor } = parse(readFileSync(new URL(url), "utf8"))
    return { format: "module", source: stripTypeScriptTypes(compileScript(descriptor, { id: "session-titles", inlineTemplate: true }).content, { mode: "transform" }), shortCircuit: true }
  },
})

const { useConnectionStore } = await import("../src/stores/connection.ts")
const { useSessionsStore } = await import("../src/stores/sessions.ts")
const { useChatStore } = await import("../src/stores/chat.ts")
const { readSummaries, readNotification, ProtocolError } = await import("../src/rpc/read.ts")
const { default: SidebarWorkspaceGroup } = await import("../src/components/SidebarWorkspaceGroup.vue")
const summary = (fields = {}) => ({ sessionId: "session", cwd: "/workspace", model: { providerId: "p", modelId: "m" }, createdAt: 1, updatedAt: 2, sequence: 3, running: false, preview: "First message", title: null, titleSource: null, titleRevision: 0, ...fields })
const title = (text, revision, source = "manual") => ({ title: text, titleSource: text === null ? null : source, titleRevision: revision })
const node = (type, text = "") => ({ type, text, children: [], props: {}, parent: undefined })
const renderer = createRenderer({
  createElement: node, createText: text => node("text", text), createComment: text => node("comment", text),
  patchProp: (element, key, _, value) => { element.props[key] = value },
  setText: (element, text) => { element.text = text },
  setElementText: (element, text) => { element.text = text; element.children = [] },
  parentNode: element => element.parent,
  nextSibling: element => element.parent?.children[element.parent.children.indexOf(element) + 1],
  insert(element, parent, anchor) {
    if (element.parent) element.parent.children.splice(element.parent.children.indexOf(element), 1)
    parent.children.splice(anchor ? parent.children.indexOf(anchor) : parent.children.length, 0, element)
    element.parent = parent
  },
  remove(element) { element.parent.children.splice(element.parent.children.indexOf(element), 1) },
})
const all = element => [element, ...element.children.flatMap(all)]
const text = element => (element.type === "comment" ? "" : element.text) + element.children.map(text).join("")
const flush = async () => { for (let index = 0; index < 8; index++) { await Promise.resolve(); await nextTick() } }

test("title metadata is required at the wire boundary and unrelated notices remain ignored", () => {
  assert.deepEqual(readSummaries([summary()]), [summary()])
  for (const fields of [{ title: undefined }, { titleSource: "other" }, { titleSource: "auto" }, { titleRevision: -1 }, { titleRevision: undefined }]) {
    assert.throws(() => readSummaries([summary(fields)]), ProtocolError)
    assert.throws(() => readNotification("session.notice", { sessionId: "session", source: "SessionStore", kind: "title", data: { ...title(null, 1), ...fields } }), ProtocolError)
  }
  assert.equal(readNotification("session.notice", { source: "Other", kind: "title" }), undefined)
  assert.equal(readNotification("session.notice", { source: "SessionStore", kind: "other" }), undefined)
})

test("owner notices update visible titles without letting delayed lists or duplicate notices roll them back", async t => {
  setActivePinia(createPinia())
  const NativeSocket = globalThis.WebSocket
  const nativeLocation = globalThis.location
  let socket
  const requests = []
  globalThis.location = { protocol: "http:", host: "fixture" }
  globalThis.WebSocket = class {
    static OPEN = 1
    readyState = 1
    constructor() { socket = this }
    send(value) { requests.push(JSON.parse(value)) }
    close() {}
  }
  const connection = useConnectionStore()
  const sessions = useSessionsStore()
  t.after(() => { connection.client.stop(); globalThis.WebSocket = NativeSocket; globalThis.location = nativeLocation })
  connection.client.start()
  const notify = (data, sessionId = "session") => socket.onmessage({ data: JSON.stringify({ jsonrpc: "2.0", method: "session.notice", params: { sessionId, source: "SessionStore", kind: "title", data } }) })
  const reply = list => {
    const request = requests.shift()
    assert.equal(request.method, "session.summaries")
    socket.onmessage({ data: JSON.stringify({ jsonrpc: "2.0", id: request.id, result: list }) })
  }
  const initial = sessions.refresh()
  notify(title("Owner title", 2, "auto"))
  notify(title("Older", 1))
  reply([summary({ updatedAt: 5 })])
  await initial
  assert.equal(sessions.byId.get("session").title, "Owner title")
  assert.equal(sessions.byId.get("session").titleSource, "auto")
  assert.equal(sessions.byId.get("session").updatedAt, 5)

  const root = node("root")
  const { h } = await import("vue")
  const app = renderer.createApp({ render: () => h(SidebarWorkspaceGroup, { workspace: sessions.workspaces[0], expanded: true, nowMs: 100 }) })
  app.directive("tooltip", {})
  app.mount(root)
  t.after(() => app.unmount())
  const label = () => all(root).find(element => element.props.class === "session-open")
  assert.match(text(label()), /Owner title/)
  assert.equal(label().props.title, "Owner title")

  const stale = sessions.refresh()
  notify(title(null, 3))
  notify(title("Duplicate", 3))
  notify(title("Wrong session", 50), "other")
  await flush()
  assert.match(text(label()), /First message/)
  reply([summary({ ...title("Stale list", 2), preview: "Updated preview", updatedAt: 10 })])
  await stale
  await flush()
  assert.equal(sessions.byId.get("session").title, null)
  assert.equal(sessions.byId.get("session").titleSource, null)
  assert.equal(sessions.byId.get("session").titleRevision, 3)
  assert.equal(sessions.byId.get("session").updatedAt, 10)
  assert.equal(label().props.title, "Updated preview")

  const newer = sessions.refresh()
  reply([summary(title("New list title", 4))])
  await newer
  await flush()
  assert.match(text(label()), /New list title/)
  const cleared = sessions.refresh()
  reply([summary({ ...title(null, 5), preview: null })])
  await cleared
  await flush()
  assert.match(text(label()), /sidebar.untitled/)
  assert.equal(sessions.byId.get("session").titleRevision, 5)
})

test("reconnect reads current titles after attaching so a rename without a listener is not lost", async t => {
  setActivePinia(createPinia())
  const NativeSocket = globalThis.WebSocket, nativeLocation = globalThis.location
  const requests = []
  let socket
  globalThis.location = { protocol: "http:", host: "fixture" }
  globalThis.WebSocket = class {
    static OPEN = 1
    readyState = 1
    constructor() { socket = markRaw(this) }
    send(value) { requests.push(JSON.parse(value)) }
    close() { this.onclose() }
  }
  const connection = useConnectionStore(), sessions = useSessionsStore(), chat = useChatStore()
  t.after(() => { connection.client.stop(); chat.$dispose(); globalThis.WebSocket = NativeSocket; globalThis.location = nativeLocation })
  const reply = (method, result) => {
    const index = requests.findIndex(request => request.method === method)
    assert.notEqual(index, -1, `Expected ${method}`)
    const [request] = requests.splice(index, 1)
    socket.onmessage({ data: JSON.stringify({ jsonrpc: "2.0", id: request.id, result }) })
  }
  const attached = async () => {
    reply("session.attach", { model: { providerId: "test", modelId: "test" } })
    await flush()
    reply("session.methods", { methods: ["session.input.pending", "session.question.pending", "session.turn.get"].map(name => ({ name })) })
    await flush()
    reply("session.branches", { branches: [] })
    await flush()
    reply("session.history", { events: [], through: 0, nextAfter: 0, done: true })
    await flush()
    reply("session.question.pending", { branchId: "main", through: 0, batches: [], next: null })
    await flush()
    reply("session.input.pending", { branchId: "main", through: 0, inputs: [], next: null })
    await flush()
    reply("session.turn.get", { branchId: "main", through: 0, turn: null, output: null })
    await flush()
  }
  const handshake = async () => {
    connection.client.start()
    socket.onopen()
    reply("hello", { instanceId: "fixture", serverVersion: "test", protocolVersion: 4, features: ["session.summaries", "session.attach", "session.history", "session.history.latestTurn", "session.methods"] })
    await flush()
  }
  await handshake()
  reply("session.summaries", [summary(title("Before reconnect", 1))])
  await flush()
  chat.open("session")
  await attached()
  connection.client.stop()
  await handshake()
  if (requests.some(request => request.method === "session.summaries")) reply("session.summaries", [summary(title("Before reconnect", 1))])
  await flush()
  await attached()
  reply("session.summaries", [summary(title("Renamed before attach", 2))])
  await flush()
  assert.equal(sessions.byId.get("session").title, "Renamed before attach")
  socket.onmessage({ data: JSON.stringify({ jsonrpc: "2.0", method: "session.notice", params: { sessionId: "session", source: "SessionStore", kind: "title", data: title("Renamed after attach", 3) } }) })
  assert.equal(sessions.byId.get("session").title, "Renamed after attach")
})

test("the sidebar leaves out the classes a picker hides, automation included", async t => {
  setActivePinia(createPinia())
  const NativeSocket = globalThis.WebSocket, nativeLocation = globalThis.location
  let socket
  const requests = []
  globalThis.location = { protocol: "http:", host: "fixture" }
  globalThis.WebSocket = class {
    static OPEN = 1
    readyState = 1
    constructor() { socket = this }
    send(value) { requests.push(JSON.parse(value)) }
    close() {}
  }
  const connection = useConnectionStore()
  const sessions = useSessionsStore()
  t.after(() => { connection.client.stop(); globalThis.WebSocket = NativeSocket; globalThis.location = nativeLocation })
  connection.client.start()
  const reply = list => {
    const request = requests.shift()
    assert.equal(request.method, "session.summaries")
    socket.onmessage({ data: JSON.stringify({ jsonrpc: "2.0", id: request.id, result: list }) })
  }
  const refresh = sessions.refresh()
  reply([
    summary({ sessionId: "chat", cwd: "/workspace", origin: "client", updatedAt: 4 }),
    summary({ sessionId: "unknown", cwd: "/workspace", origin: "unknown", updatedAt: 3 }),
    summary({ sessionId: "internal", cwd: "/workspace", origin: "internal", updatedAt: 2 }),
    summary({ sessionId: "automation", cwd: "/workspace", origin: "automation", updatedAt: 1 }),
  ])
  await refresh
  const shown = sessions.workspaces.flatMap(workspace => workspace.sessions.map(session => session.sessionId))
  assert.deepEqual(shown, ["chat", "unknown"])
  assert.equal(sessions.byId.get("automation").sessionId, "automation")
})
