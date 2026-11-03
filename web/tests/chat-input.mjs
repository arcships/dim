import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { registerHooks, stripTypeScriptTypes } from "node:module"
import test from "node:test"
import { compileScript, parse } from "vue/compiler-sfc"
import { createRenderer, nextTick, reactive } from "vue"
import { createPinia, setActivePinia } from "pinia"

globalThis.Document = class {}
globalThis.ShadowRoot = class {}

const components = /\/(ChatPage|LineConversation|ComposerPanel|ComposerPermissionMenu|ConfirmDialog|QuestionPrompt)\.vue$/
registerHooks({
  resolve(specifier, context, next) {
    if (specifier.startsWith("../../stores/") || (context.parentURL?.endsWith("/stores/chat.ts") && ["./connection", "./sessions", "./ui"].includes(specifier))) {
      const store = specifier.split("/").at(-1)
      const name = `use${store[0].toUpperCase()}${store.slice(1)}Store`
      const source = `export const ${name}=()=>globalThis.chatInputStores.${store}; export const workspaceName=x=>x`
      return { url: `data:text/javascript,${encodeURIComponent(source)}`, shortCircuit: true }
    }
    if (specifier === "vue-i18n") return { url: "data:text/javascript,export const useI18n=()=>({t:key=>key})", shortCircuit: true }
    if (specifier === "vue-router") return { url: "data:text/javascript,export const useRoute=()=>({params:{}});export const useRouter=()=>({push:async()=>{}});export const RouterLink={render:()=>null}", shortCircuit: true }
    if (specifier.endsWith(".vue") && !components.test(specifier)) return { url: "data:text/javascript,export default {render:()=>null}", shortCircuit: true }
    try { return next(specifier, context) } catch (error) {
      if (specifier.startsWith(".") && !/\.(ts|vue)$/.test(specifier)) return next(`${specifier}.ts`, context)
      throw error
    }
  },
  load(url, context, next) {
    if (url.endsWith(".ts")) return { format: "module", source: stripTypeScriptTypes(readFileSync(new URL(url), "utf8"), { mode: "transform" }), shortCircuit: true }
    if (!components.test(url)) return next(url, context)
    const { descriptor } = parse(readFileSync(new URL(url), "utf8"))
    return { format: "module", source: stripTypeScriptTypes(compileScript(descriptor, { id: "chat-input-test", inlineTemplate: true }).content, { mode: "transform" }), shortCircuit: true }
  },
})

const { default: ChatPage } = await import("../src/features/chat/ChatPage.vue")
const { default: LineConversation } = await import("../src/features/lines/LineConversation.vue")
const { default: ComposerPanel } = await import("../src/features/chat/ComposerPanel.vue")
const { useChatStore } = await import("../src/stores/chat.ts")
const { readNotification } = await import("../src/rpc/read.ts")
const { RpcError } = await import("../src/rpc/client.ts")
const node = (type, text = "") => ({
  type, text, children: [], props: {}, parent: undefined, style: {}, value: "", scrollHeight: 40,
  addEventListener() {}, focus() {}, getRootNode() { return {} },
  setSelectionRange(start) { this.selectionStart = start },
  showModal() { this.props.open = true }, close() { this.props.open = false },
})
const renderer = createRenderer({
  createElement: node, createText: (text) => node("text", text), createComment: (text) => node("comment", text),
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
const flush = async () => { for (let i = 0; i < 8; i++) { await Promise.resolve(); await nextTick() } }
const click = (element) => element.props.onClick({ stopPropagation() {} })
const question = (branchId, callId, multiSelect = false) => ({
  branchId, requestId: "batch", callId,
  questions: [{ id: "1", question: `Choose ${callId}`, options: [{ label: "Alpha" }, { label: "Beta" }], multiSelect, allowCustom: true }],
})

function mount(t, branchId, questions = [], component, stores = {}) {
  const replies = [], submissions = []
  const view = reactive({
    status: "ready", rows: [], running: true, stopping: false, sessionRunning: true,
    approvals: [], questions, methods: ["attachment.put", "session.files.search"], queued: [],
    lines: [{ id: "worker", purpose: "Worker", parentId: "main", state: { kind: "working" } }],
    branches: { worker: { status: "ready", rows: [], running: true } },
  })
  globalThis.chatInputStores = {
    chat: {
      views: { session: view }, open() {}, ensureLine() {},
      async submit(sessionId, branch, value, attachments) { submissions.push({ sessionId, branch, value, attachments }) },
      async answerQuestion(sessionId, ask, answers) { replies.push({ sessionId, branch: ask.branchId, callId: ask.callId, answers }) },
      searchFiles: async () => [
        { relativePath: "report.md", absolutePath: "/repo/report.md", dir: false },
        { relativePath: "report.md copy", absolutePath: "/repo/report.md copy", dir: false },
      ],
    },
    connection: { supports: () => false },
    models: { available: false, load: async () => {} },
    sessions: { byId: new Map() },
    toast: { push() {} },
    ui: { shellMode: "wide", approvalKeyboard: () => false },
    ...stores,
  }
  const root = node("root")
  const app = renderer.createApp(component ?? (branchId === "main" ? ChatPage : LineConversation), {
    sessionId: "session", branchId,
    ...(component ? { running: false, modelSelectable: false, model: undefined, attachments: false, send: async () => {}, selectModel() {} } : {}),
  })
  app.directive("tooltip", {})
  app.mount(root)
  t.after(() => app.unmount())
  return { root, view, replies, submissions }
}

async function permissionSession(t, policy) {
  setActivePinia(createPinia())
  const chat = useChatStore()
  t.after(() => chat.$dispose())
  const server = { policy, version: 3, updates: [], failure: undefined }
  const connection = {
    supports: () => true,
    client: { async call(method, params, read) {
      if (method === "session.attach") return read({})
      if (method === "session.methods") return read({ methods: ["session.permission.get", "session.permission.update"].map((name) => ({ name })) })
      if (method === "session.history") return read({ events: [], through: 0, nextAfter: 0, done: true })
      if (method === "session.permission.get") return read({ policy: server.policy, version: server.version })
      assert.equal(method, "session.permission.update")
      server.updates.push(structuredClone(params))
      if (server.failure) throw server.failure()
      assert.equal(params.expectedVersion, server.version)
      server.policy = structuredClone(params.policy)
      return read({ version: ++server.version })
    } },
  }
  const { root } = mount(t, "main", [], undefined, { chat, connection })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  const option = (label) => all(root).find((element) => element.type === "button" && element.children.some((child) => child.children.some((span) => text(span) === label)))
  return { root, chat, server, option }
}

test("permission presets preserve tool exceptions and confirm full access", async (t) => {
  const initial = { preset: "workspace-write", allowedTools: ["read"], disallowedTools: ["exec"] }
  const { root, chat, server, option } = await permissionSession(t, initial)
  const confirmation = () => all(root).find((element) => element.type === "dialog" && element.props.open)
  assert.equal(option("composer.permission.preset.workspace-write").props["aria-pressed"], true)
  assert.match(text(root), /composer.permission.allowedTools: read/)
  assert.match(text(root), /composer.permission.disallowedTools: exec/)
  await click(option("composer.permission.preset.workspace-write")); await flush()
  assert.equal(server.updates.length, 0)
  await click(option("composer.permission.preset.read-only")); await flush()
  assert.deepEqual(server.policy, { ...initial, preset: "read-only" })
  assert.deepEqual(chat.views.session.permission, server.policy)
  assert.equal(option("composer.permission.preset.read-only").props["aria-pressed"], true)
  const cancelled = click(option("composer.permission.preset.full-access"))
  await flush()
  await click(button(confirmation(), "cancel")); await cancelled; await flush()
  assert.equal(server.updates.length, 1)
  const widening = click(option("composer.permission.preset.full-access"))
  await flush()
  await click(button(confirmation(), "confirm")); await widening; await flush()
  assert.deepEqual(server.policy, { ...initial, preset: "full-access" })
  assert(server.updates.every((update) => update.sessionId === "session" && update.branchId === "main"))
})

test("permission changes refresh live presets and conflicts without retrying or changing tool lists", async (t) => {
  const initial = { preset: "read-only", allowedTools: [], disallowedTools: ["exec"] }
  const { chat, server, option } = await permissionSession(t, initial)
  const remote = { ...initial, preset: "full-access" }
  chat.receive(readNotification("session.event", { sessionId: "session", branchId: "main", event: { id: "remote", sequence: 1, create_at: 1, type: "permission", name: "changed", data: { policy: remote } } }))
  await new Promise((resolve) => setTimeout(resolve, 20)); await flush()
  assert.deepEqual(chat.views.session.permission, remote)
  assert.equal(option("composer.permission.preset.full-access").props["aria-pressed"], true)
  const concurrent = { ...initial, allowedTools: ["read"] }
  server.failure = () => {
    server.policy = concurrent
    server.version++
    return new RpcError(-32000, "changed elsewhere", "permission_conflict")
  }
  assert.equal(await chat.setPermission("session", "workspace-write"), "conflict")
  assert.equal(server.updates.length, 1, "a conflict refreshes without retrying the write")
  assert.deepEqual(chat.views.session.permission, concurrent)
  server.failure = () => new RpcError(-32602, "invalid policy", undefined)
  await assert.rejects(chat.setPermission("session", "workspace-write"), /invalid policy/)
  assert.equal(server.updates.length, 2)
  assert.deepEqual(chat.views.session.permission, server.policy)
})

for (const branchId of ["main", "worker"]) {
  test(`${branchId}: all calls of a question batch can be answered before settlement`, async (t) => {
    const { root, view, replies } = mount(t, branchId, [question(branchId, "first"), question(branchId, "second"), question("elsewhere", "hidden")])
    const cards = () => all(root).filter((element) => element.type === "article")
    assert.equal(cards().length, 2)
    for (const card of cards()) {
      all(card).find((element) => element.props.type === "radio").props.onChange()
      await flush()
      assert.equal(button(card, "question.submit").props.disabled, false)
      await click(button(card, "question.submit"))
      await flush()
      assert.equal(button(card, "question.submit").props.disabled, true)
    }
    assert.deepEqual(replies, ["first", "second"].map((callId) => ({ sessionId: "session", branch: branchId, callId, answers: [{ questionId: "1", selected: ["Alpha"] }] })))
    assert.equal(cards().length, 2, "accepted answers remain visible until the batch settles")
    view.questions = []
    await flush()
    assert.equal(cards().length, 0)
    assert(all(root).some((element) => element.type === "textarea"))
  })

  test(`${branchId}: attachment-only drafts send images and long pastes during a run`, async (t) => {
    const original = globalThis.FileReader
    globalThis.FileReader = class { readAsDataURL() { this.result = "data:image/png;base64,aW1hZ2U="; this.onload() } }
    t.after(() => { globalThis.FileReader = original })
    const { root, submissions } = mount(t, branchId)
    const field = all(root).find((element) => element.type === "textarea")
    let prevented = false
    field.props.onPaste({ clipboardData: { files: [{ type: "image/png" }] }, preventDefault() { prevented = true } })
    await flush()
    assert.equal(prevented, true)
    assert(all(root).some((element) => element.type === "img"))
    const paste = "long paste\n".repeat(1700)
    field.props.onPaste({ clipboardData: { files: [], getData: () => paste }, preventDefault() {} })
    await flush()
    const send = button(root, "composer.send")
    assert.equal(send.props.disabled, false)
    await click(send)
    assert.deepEqual(submissions, [{ sessionId: "session", branch: branchId, value: "", attachments: { images: [{ type: "image", source: { type: "base64", mediaType: "image/png", data: "aW1hZ2U=" } }], pastes: [paste] } }])
  })
}

test("overlapping image reads preserve additions and removals in either completion order", async (t) => {
  const original = globalThis.FileReader
  const readers = []
  globalThis.FileReader = class {
    readAsDataURL(file) { this.result = `data:image/png;base64,${file.data}`; readers.push(this) }
  }
  t.after(() => { globalThis.FileReader = original })
  for (const order of [[0, 1], [1, 0]]) {
    const { root, submissions } = mount(t, "main")
    const field = all(root).find((element) => element.type === "textarea")
    const paste = (data) => field.props.onPaste({ clipboardData: { files: [{ type: "image/png", data }] }, preventDefault() {} })
    paste("b2xk")
    readers.shift().onload()
    await flush()
    paste("Zmlyc3Q=")
    paste("c2Vjb25k")
    await click(button(root, "composer.removeAttachment"))
    for (const index of order) {
      readers[index].onload()
      await flush()
    }
    readers.length = 0
    assert.equal(all(root).filter((element) => element.type === "img").length, 2)
    await click(button(root, "composer.send"))
    assert.deepEqual(submissions[0].attachments.images.map((image) => image.source.data), order.map((index) => ["Zmlyc3Q=", "c2Vjb25k"][index]))
  }
})

test("send acknowledgments clear only submitted attachments, while failures preserve the whole draft", async (t) => {
  const original = globalThis.FileReader
  globalThis.FileReader = class {
    readAsDataURL(file) { this.result = `data:image/png;base64,${file.data}`; this.onload() }
  }
  t.after(() => { globalThis.FileReader = original })
  for (const accepted of [true, false]) {
    const { root, submissions } = mount(t, "main")
    const field = all(root).find((element) => element.type === "textarea")
    const pastes = ["first paste\n".repeat(1700), "next paste\n".repeat(1800)]
    const add = async (data, value) => {
      field.props.onPaste({ clipboardData: { files: [{ type: "image/png", data }] }, preventDefault() {} })
      field.props.onPaste({ clipboardData: { files: [], getData: () => value }, preventDefault() {} })
      await flush()
    }
    await add("Zmlyc3Q=", pastes[0])
    const submit = globalThis.chatInputStores.chat.submit
    let settle
    globalThis.chatInputStores.chat.submit = async (...args) => {
      await submit(...args)
      await new Promise((resolve, reject) => { settle = () => accepted ? resolve() : reject(new Error("offline")) })
    }
    const pending = click(button(root, "composer.send"))
    await flush()
    field.props["onUpdate:modelValue"]("next message")
    await add("c2Vjb25k", pastes[1])
    settle()
    await pending
    await flush()
    assert.deepEqual(submissions[0].attachments.images.map((image) => image.source.data), ["Zmlyc3Q="])
    assert.deepEqual(submissions[0].attachments.pastes, [pastes[0]])
    assert.equal(all(root).filter((element) => element.type === "img").length, accepted ? 1 : 2)
    globalThis.chatInputStores.chat.submit = submit
    await click(button(root, "composer.send"))
    assert.equal(submissions[1].value, "next message")
    assert.deepEqual(submissions[1].attachments.images.map((image) => image.source.data), accepted ? ["c2Vjb25k"] : ["Zmlyc3Q=", "c2Vjb25k"])
    assert.deepEqual(submissions[1].attachments.pastes, accepted ? [pastes[1]] : pastes)
    assert.equal(button(root, "composer.removeAttachment"), undefined)
  }
})

test("single-choice answers switch between options and custom text; multiple-choice answers combine them", async (t) => {
  for (const mode of ["custom", "option", "multiple"]) {
    const { root, replies } = mount(t, "main", [question("main", mode, mode === "multiple")])
    const option = all(root).find((element) => element.props.type === (mode === "multiple" ? "checkbox" : "radio"))
    const custom = all(root).find((element) => element.props.type === "text")
    if (mode !== "option") option.props.onChange()
    custom.props.onInput({ target: { value: "Gamma" } })
    if (mode === "option") option.props.onChange()
    await flush()
    await click(button(root, "question.submit"))
    assert.deepEqual(replies[0].answers, [{ questionId: "1", selected: mode === "custom" ? [] : ["Alpha"], ...(mode === "option" ? {} : { custom: "Gamma" }) }])
  }
})

test("explicitly disabled attachments leave the clipboard's native paste behavior intact", (t) => {
  const { root } = mount(t, "main", [], ComposerPanel)
  const field = all(root).find((element) => element.type === "textarea")
  field.props.onPaste({ clipboardData: { files: [{ type: "image/png" }] }, preventDefault() { assert.fail("attachments are disabled") } })
  assert(!all(root).some((element) => element.type === "img"))
})

test("files picked with overlapping names keep their own paths in the submitted message", async (t) => {
  const { root, submissions } = mount(t, "main")
  const field = all(root).find((element) => element.type === "textarea")
  field.props.onFocus()
  for (const [draft, name] of [["@report", "report.md"], ["@report.md @report", "report.md copy"]]) {
    field.value = draft
    field.selectionStart = draft.length
    field.props["onUpdate:modelValue"](draft)
    await flush()
    field.props.onInput()
    await new Promise((resolve) => setTimeout(resolve, 100))
    await flush()
    const candidate = all(root).find((element) => element.props.role === "option" && text(element) === name)
    assert(candidate)
    await click(candidate)
    await flush()
  }
  await click(button(root, "composer.send"))
  assert.equal(submissions[0].value, "[@report.md](/repo/report.md) [@report.md copy](</repo/report.md copy>) ")
})
