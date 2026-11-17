import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { registerHooks, stripTypeScriptTypes } from "node:module"
import test from "node:test"
import { compileScript, parse } from "vue/compiler-sfc"
import { createRenderer, nextTick, reactive } from "vue"
import { createPinia, setActivePinia } from "pinia"
import { nuxtUiTestPlugin } from "./helpers/nuxt-ui.mjs"

globalThis.Document = class {}
globalThis.ShadowRoot = class {}
/** Every observer the components create, so a test can fire one by hand. */
globalThis.chatInputObservers = []
globalThis.ResizeObserver = class {
  constructor(callback) { this.callback = callback; this.observed = [] }
  observe(target) { this.observed.push(target); globalThis.chatInputObservers.push(this) }
  disconnect() { this.observed = [] }
}

const components = /\/(AppearanceSettings|ChatPage|LineConversation|LinesPanel|ComposerPanel|ComposerPermissionMenu|ConfirmDialog|QuestionPrompt|TimelineUserMessage)\.vue$/
registerHooks({
  resolve(specifier, context, next) {
    if (specifier.startsWith("../../stores/") || (context.parentURL?.endsWith("/stores/chat.ts") && ["./connection", "./sessions", "./ui"].includes(specifier))) {
      const store = specifier.split("/").at(-1)
      const name = store === "models" ? "useModels" : `use${store[0].toUpperCase()}${store.slice(1)}Store`
      const source = `export const ${name}=()=>globalThis.chatInputStores.${store}; export const workspaceName=x=>x`
      return { url: `data:text/javascript,${encodeURIComponent(source)}`, shortCircuit: true }
    }
    if (specifier === "vue-i18n") return { url: "data:text/javascript,export const createI18n=()=>({global:{t:key=>key,locale:{value:\"en\"},setLocaleMessage(){},availableLocales:[]}});export const useI18n=()=>({t:key=>key,locale:{value:'en'}})", shortCircuit: true }
    if (specifier === "vue-router") return { url: "data:text/javascript,export const useRoute=()=>({params:{}});export const useRouter=()=>({push:async to=>globalThis.chatInputNavigations.push(to)});export const RouterLink={render:()=>null}", shortCircuit: true }
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
const { default: TimelineUserMessage } = await import("../src/features/chat/TimelineUserMessage.vue")
const { default: AppearanceSettings } = await import("../src/features/settings/AppearanceSettings.vue")
const { useChatStore } = await import("../src/stores/chat.ts")
const { readNotification } = await import("../src/rpc/read.ts")
const { RpcError } = await import("../src/rpc/client.ts")
const node = (type, text = "") => ({
  type, text, children: [], props: {}, parent: undefined, style: {}, value: "",
  /** An element in no document has no layout: a browser reports nothing for it, and so does this. */
  get scrollHeight() {
    let element = this
    while (element.parent !== undefined) element = element.parent
    return element.type === "root" ? 40 : 0
  },
  addEventListener() {}, focus() { this.focuses = (this.focuses ?? 0) + 1 }, scrollTo() {}, getRootNode() { return {} },
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
const flush = async () => { await new Promise(setImmediate); await nextTick() }
const click = (element) => element.props.onClick({ stopPropagation() {} })
const submitQuestion = (root) => all(root).find((element) => element.type === "form").props.onSubmit({ preventDefault() {} })
const question = (branchId, callId, multiSelect = false) => ({
  branchId, requestId: "batch", callId,
  questions: [{ id: "1", question: `Choose ${callId}`, options: [{ label: "Alpha" }, { label: "Beta" }], multiSelect, allowCustom: true }],
})

function mount(t, branchId, questions = [], component, stores = {}, props = {}, sharedView) {
  const replies = [], submissions = []
  globalThis.chatInputNavigations = []
  globalThis.chatInputStops = []
  globalThis.chatInputWithdrawals = []
  const view = sharedView ?? reactive({
    status: "ready", rows: [], running: true, stopping: false, sessionRunning: true,
    approvals: [], questions, methods: ["attachment.put", "session.files.search"], queued: [],
    lines: [{ id: "worker", purpose: "Worker", parentId: "main", state: { kind: "working" } }],
    branches: { worker: { status: "ready", rows: [], running: true } },
  })
  globalThis.chatInputStores = {
    chat: {
      views: { session: view }, open() {}, ensureLine() {},
      async submit(sessionId, branch, value, attachments, mode) { submissions.push({ sessionId, branch, value, attachments, mode }) },
      async stop(sessionId, branch) { globalThis.chatInputStops.push({ sessionId, branch }) },
      async withdrawInput(sessionId, branch, inputId) {
        globalThis.chatInputWithdrawals.push({ sessionId, branch, inputId })
        return { status: "deleted" }
      },
      async answerQuestion(sessionId, ask, answers) { replies.push({ sessionId, branch: ask.branchId, callId: ask.callId, answers }) },
      searchFiles: async () => [
        { relativePath: "report.md", absolutePath: "/repo/report.md", dir: false },
        { relativePath: "report.md copy", absolutePath: "/repo/report.md copy", dir: false },
      ],
    },
    connection: { supports: () => false },
    composer: reactive({ sendMode: "steer", alternate() { return this.sendMode === "steer" ? "queue" : "steer" } }),
    models: { available: false, load: async () => {}, clear: () => {} },
    sessions: { byId: new Map(), summaries: [] },
    toast: { push() {} },
    ui: { shellMode: "wide", approvalKeyboard: () => false },
    ...stores,
  }
  const root = node("root")
  const app = renderer.createApp(component ?? (branchId === "main" ? ChatPage : LineConversation), {
    sessionId: "session", branchId,
    ...(component ? { running: false, modelSelectable: false, model: undefined, attachments: false, send: async () => {}, selectModel() {} } : {}),
    ...props,
  })
  app.use(nuxtUiTestPlugin)
  app.mount(root)
  t.after(() => app.unmount())
  return { root, view, replies, submissions, unmount: () => app.unmount() }
}

async function permissionSession(t, policy) {
  setActivePinia(createPinia())
  const chat = useChatStore()
  t.after(() => chat.$dispose())
  const server = { policy, version: 3, updates: [], failure: undefined }
  const connection = {
    supports: (method) => ["session.history.latestTurn", "session.methods", "session.permission.get", "session.permission.update"].includes(method),
    client: { async call(method, params, read) {
      if (method === "session.attach") return read({ model: { providerId: "test", modelId: "test" } })
      if (method === "session.methods") return read({ methods: ["session.input.pending", "session.question.pending", "session.turn.get"].map((name) => ({ name })) })
      if (method === "session.branches") return read({ branches: [] })
      if (method === "session.input.pending") return read({ ...params, inputs: [], next: null })
      if (method === "session.question.pending") return read({ ...params, batches: [], next: null })
      if (method === "session.turn.get") return read({ ...params, turn: null, output: null })
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
  const confirmation = () => all(root).find((element) => element.props.role === "dialog")
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
  test(`${branchId}: questions advance individually and retain edits through review and retry`, async (t) => {
    const ask = question(branchId, "steps")
    ask.questions.push({ id: "2", header: "Features", question: "Choose features", options: [{ label: "Audit" }, { label: "Export" }], multiSelect: true, allowCustom: true })
    const { root, replies } = mount(t, branchId, [ask])
    const inputs = () => all(root).filter((element) => element.type === "input")
    assert.equal(all(root).filter((element) => element.type === "fieldset").length, 1)
    assert.doesNotMatch(text(root), /Choose features/)
    assert.equal(button(root, "question.next").props.disabled, true)
    inputs()[1].props.onChange()
    await submitQuestion(root); await flush()
    assert.match(text(root), /Choose features/)
    assert.doesNotMatch(text(root), /Choose steps/)
    assert.equal(replies.length, 0)
    inputs()[0].props.onChange()
    inputs()[1].props.onChange()
    inputs()[2].props.onInput({ target: { value: "  Backups  " } })
    await flush()
    await click(button(root, "question.previous")); await flush()
    assert.equal(inputs()[1].props.checked, true, "the first answer survives navigation")
    inputs()[2].props.onInput({ target: { value: "PostgreSQL" } })
    await submitQuestion(root); await flush()
    assert.equal(inputs()[0].props.checked, true)
    assert.equal(inputs()[1].props.checked, true)
    assert.equal(inputs()[2].props.value, "  Backups  ")
    await submitQuestion(root); await flush()
    assert.equal(all(root).filter((element) => element.type === "fieldset").length, 0)
    assert.match(text(root), /PostgreSQL/)
    assert.match(text(root), /Audit, Export, Backups/)
    globalThis.chatInputStores.chat.answerQuestion = async () => { throw new RpcError(-32602, "Retry this answer", "invalid_params") }
    await submitQuestion(root); await flush()
    assert.match(text(root), /Retry this answer/)
    assert.equal(button(root, "question.submit").props.disabled, false)
    globalThis.chatInputStores.chat.answerQuestion = async (sessionId, ask, answers) => { replies.push({ sessionId, branch: ask.branchId, answers }) }
    await submitQuestion(root); await flush()
    assert.deepEqual(replies, [{ sessionId: "session", branch: branchId, answers: [
      { questionId: "1", selected: [], custom: "PostgreSQL" },
      { questionId: "2", selected: ["Audit", "Export"], custom: "Backups" },
    ] }])
    assert.equal(button(root, "question.submit").props.disabled, true)
  })

  test(`${branchId}: all calls of a question batch can be answered before settlement`, async (t) => {
    const { root, view, replies } = mount(t, branchId, [question(branchId, "first"), question(branchId, "second"), question("elsewhere", "hidden")])
    const cards = () => all(root).filter((element) => element.type === "article")
    assert.equal(cards().length, 2)
    for (const card of cards()) {
      all(card).find((element) => element.props.type === "radio").props.onChange()
      await flush()
      assert.equal(button(card, "question.submit").props.disabled, false)
      await submitQuestion(card)
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
    const image = new File(["image"], "image.png", { type: "image/png" })
    const { root, submissions } = mount(t, branchId)
    const field = all(root).find((element) => element.type === "textarea")
    let prevented = false
    field.props.onPaste({ clipboardData: { files: [image] }, preventDefault() { prevented = true } })
    await flush()
    assert.equal(prevented, true)
    assert(all(root).some((element) => element.type === "img"))
    const paste = "long paste\n".repeat(1700)
    field.props.onPaste({ clipboardData: { files: [], getData: () => paste }, preventDefault() {} })
    await flush()
    const send = button(root, "composer.send")
    assert.equal(send.props.disabled, false)
    await click(send)
    const { onProgress, ...attachments } = submissions[0].attachments
    assert.equal(typeof onProgress, "function")
    assert.deepEqual({ ...submissions[0], attachments }, { sessionId: "session", branch: branchId, value: "", attachments: { images: [image], pastes: [paste] }, mode: "steer" })
  })
}

test("adding and removing file attachments preserves selection order", async (t) => {
  const { root, submissions } = mount(t, "main")
  const field = all(root).find((element) => element.type === "textarea")
  for (const name of ["old", "first", "second"]) {
    field.props.onPaste({ clipboardData: { files: [new File([name], name, { type: "image/png" })] }, preventDefault() {} })
    await flush()
  }
  await click(button(root, "composer.removeAttachment"))
  await flush()
  assert.equal(all(root).filter((element) => element.type === "img").length, 2)
  await click(button(root, "composer.send"))
  assert.deepEqual(submissions[0].attachments.images.map((image) => image.name), ["first", "second"])
})

test("send acknowledgments clear only submitted attachments, while failures preserve the whole draft", async (t) => {
  for (const accepted of [true, false]) {
    const { root, submissions } = mount(t, "main")
    const field = all(root).find((element) => element.type === "textarea")
    const pastes = ["first paste\n".repeat(1700), "next paste\n".repeat(1800)]
    const add = async (data, value) => {
      field.props.onPaste({ clipboardData: { files: [new File([data], data, { type: "image/png" })] }, preventDefault() {} })
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
    assert.deepEqual(submissions[0].attachments.images.map((image) => image.name), ["Zmlyc3Q="])
    assert.deepEqual(submissions[0].attachments.pastes, [pastes[0]])
    assert.equal(all(root).filter((element) => element.type === "img").length, accepted ? 1 : 2)
    globalThis.chatInputStores.chat.submit = submit
    await click(button(root, "composer.send"))
    assert.equal(submissions[1].value, "next message")
    assert.deepEqual(submissions[1].attachments.images.map((image) => image.name), accepted ? ["c2Vjb25k"] : ["Zmlyc3Q=", "c2Vjb25k"])
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
    await submitQuestion(root)
    assert.deepEqual(replies[0].answers, [{ questionId: "1", selected: mode === "custom" ? [] : ["Alpha"], ...(mode === "option" ? {} : { custom: "Gamma" }) }])
  }
})

test("explicitly disabled attachments leave the clipboard's native paste behavior intact", (t) => {
  const { root } = mount(t, "main", [], ComposerPanel)
  const field = all(root).find((element) => element.type === "textarea")
  field.props.onPaste({ clipboardData: { files: [{ type: "image/png" }] }, preventDefault() { assert.fail("attachments are disabled") } })
  assert(!all(root).some((element) => element.type === "img"))
})

test("schedule commands navigate only when admitted and not escaped", async t => {
  for (const command of ["/schedule", "/scheduled"]) {
    for (const [admitted, escaped] of [[true, false], [true, true], [false, false]]) {
      const { root, submissions } = mount(t, "main", [], undefined, { connection: { supports: method => admitted && method === "schedule.list" } })
      const field = all(root).find(element => element.type === "textarea")
      field.props.onFocus()
      field.props["onUpdate:modelValue"](command)
      await flush()
      const key = key => field.props.onKeydown({ key, preventDefault() {}, stopPropagation() {} })
      if (escaped) key("Escape")
      await flush()
      key("Enter")
      await flush()
      if (admitted && !escaped) {
        assert.deepEqual(globalThis.chatInputNavigations, [{ name: "settings-scheduled" }])
        assert.deepEqual(submissions, [])
      } else {
        assert.deepEqual(globalThis.chatInputNavigations, [])
        assert.equal(submissions[0].value, command)
      }
    }
  }
})

test("a /loop submission reaches the loop callback, and is offered only when one is given", async (t) => {
  const calls = []
  const loop = async (words) => { calls.push(words) }
  const { root, submissions } = mount(t, "main", [], ComposerPanel, {}, { sessionCommands: { loop } })
  const field = all(root).find((element) => element.type === "textarea")
  field.props.onFocus()
  field.props["onUpdate:modelValue"]("/loop 5m check")
  await flush()
  field.props.onKeydown({ key: "Enter", preventDefault() {}, stopPropagation() {} })
  await flush()
  assert.deepEqual(calls, ["5m check"], "the words after /loop reach the callback")
  assert.deepEqual(submissions, [], "the command is not sent as model input")

  const bare = mount(t, "main", [], ComposerPanel, {}, { sessionCommands: { loop } })
  const bareField = all(bare.root).find((element) => element.type === "textarea")
  const enter = () => bareField.props.onKeydown({ key: "Enter", preventDefault() {}, stopPropagation() {} })
  bareField.props.onFocus()
  bareField.props["onUpdate:modelValue"]("/loop")
  await flush()
  await enter()
  await flush()
  assert.equal(bareField.value, "/loop ", "the menu completes the command without dispatching it")
  await enter()
  await flush()
  assert.deepEqual(calls, ["5m check", ""], "a bare /loop asks for the line's loops")

  const absent = mount(t, "main", [], ComposerPanel)
  const absentField = all(absent.root).find((element) => element.type === "textarea")
  absentField.props.onFocus()
  absentField.props["onUpdate:modelValue"]("/loop")
  await flush()
  assert.equal(
    all(absent.root).some((element) => element.type === "button" && text(element).startsWith("/loop")),
    false,
    "without a callback /loop is not offered",
  )
})

test("a /loop stop without an ID stops a loop only when it is the line's only live one", async (t) => {
  const stops = [], toasts = []
  const live = [
    { loopId: "newest", state: "running", round: 3, nextAt: "2026-11-13T09:41:00.000Z", prompt: "poll" },
    { loopId: "older", state: "waiting", round: 2, nextAt: null, prompt: "wait" },
  ]
  const stopWith = async (words, listed) => {
    const { root, view } = mount(t, "main", [], undefined, {
      toast: { push(toast) { toasts.push(toast) } },
    })
    view.methods = ["session.loop.start", "session.loop.list", "session.loop.stop"]
    const chat = globalThis.chatInputStores.chat
    chat.readLoops = async () => listed
    chat.stopLoop = async (sessionId, loopId) => { stops.push({ sessionId, loopId }) }
    const field = all(root).find((element) => element.type === "textarea")
    field.props.onFocus()
    field.props["onUpdate:modelValue"](words)
    await flush()
    field.props.onKeydown({ key: "Enter", preventDefault() {}, stopPropagation() {} })
    await flush()
  }

  await stopWith("/loop stop", live)
  assert.deepEqual(stops, [], "two live loops stop nothing")
  assert.deepEqual(toasts.map((toast) => toast.title), ["loop.noSingle"])

  stops.length = 0; toasts.length = 0
  await stopWith("/loop stop", [live[1], { ...live[0], state: "stopped" }])
  assert.deepEqual(stops, [{ sessionId: "session", loopId: "older" }], "the line's only live loop is stopped")
  assert.deepEqual(toasts.map((toast) => toast.title), ["loop.stopped"])

  stops.length = 0; toasts.length = 0
  await stopWith("/loop stop newest", live)
  assert.deepEqual(stops, [{ sessionId: "session", loopId: "newest" }], "an explicit ID stops that loop")
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


test("image submission uploads the File before sending only its durable reference", async t => {
  const calls = [], uploads = []
  globalThis.chatInputStores = { connection: { client: { async call(method, params, read) {
    calls.push({ method, params }); return read({})
  } } } }
  const previousFetch = globalThis.fetch
  const previousXHR = globalThis.XMLHttpRequest
  t.after(() => { globalThis.fetch = previousFetch; globalThis.XMLHttpRequest = previousXHR })
  globalThis.XMLHttpRequest = class {
    upload = {}
    open(method, url) { this.method = method; this.url = url }
    async send(body) {
      this.upload.onprogress({ lengthComputable: true, loaded: 5, total: 10 })
      const response = await globalThis.fetch(this.url, { method: this.method, body })
      this.status = response.status
      this.responseText = await response.text()
      this.upload.onprogress({ lengthComputable: true, loaded: 10, total: 10 })
      this.onload()
    }
  }
  globalThis.fetch = async (url, request) => {
    assert.equal(calls.length, 0)
    uploads.push({ url, request })
    assert(request.body instanceof FormData)
    assert.equal(await request.body.get("file").text(), "image bytes")
    return Response.json({ attachmentId: "image-1", path: "/files/image-1/image", bytes: 11,
      image: { type: "file", mediaType: "image/png", fileId: "image-1" } })
  }
  setActivePinia(createPinia())
  const chat = useChatStore()
  t.after(() => chat.$dispose())
  const image = new File(["image bytes"], "screen.png", { type: "image/png" })
  const progress = []
  await chat.submit("session", "main", "look", { images: [image], pastes: [], onProgress: value => progress.push(value) })
  assert(progress.includes(50))
  assert.equal(progress.at(-1), 100)
  assert.equal(uploads[0].url, "/api/sessions/session/files?image=true")
  assert.equal(calls[0].method, "session.submit")
  assert.deepEqual(calls[0].params.user.content, [{ type: "text", text: "look" },
    { type: "image", source: { type: "file", mediaType: "image/png", fileId: "image-1" } }])
  globalThis.fetch = async () => new Response("failed", { status: 500 })
  await assert.rejects(chat.submit("session", "main", "again", { images: [new File(["new bytes"], "new.png")], pastes: [] }), /upload failed/)
  assert.equal(calls.length, 1)
  await chat.submit("session", "main", "retry", { images: [image], pastes: [] })
  assert.equal(uploads.length, 1, "retrying a draft reuses its accepted file identity")
  assert.equal(calls[1].params.user.content[1].source.fileId, "image-1")
  assert.equal(calls[1].params.mode, "queue", "a submit with no mode asked for holds the message")

  const restored = { at: 8, image: { type: "image", source: { type: "file", mediaType: "image/png", fileId: "recalled" } } }
  await chat.submit("session", "main", "send now", { images: [], pastes: [], restored: [restored] }, "steer")
  assert.equal(uploads.length, 1, "a recalled image uploads nothing")
  assert.equal(calls[2].params.mode, "steer")
  assert.deepEqual(calls[2].params.user.content, [{ type: "text", text: "send now" }, restored.image], "a recalled image keeps its stored file reference and the draft offset it stood at")
})

test("the /skill: namespace offers Skills, fills the token, and sends it as a message", async (t) => {
  const skills = [{ name: "commit", description: "Writes commit messages" }, { name: "deploy", description: "Ships a build" }]
  const { root, submissions } = mount(t, "main", [], ComposerPanel, {}, { skills })
  const field = all(root).find((element) => element.type === "textarea")
  field.props.onFocus()
  field.props["onUpdate:modelValue"]("/skill:")
  await flush()
  const labels = all(root).filter((element) => element.type === "button").map(text)
  const offered = labels.filter((label) => label.includes("/skill:"))
  assert.equal(offered.length, 2, `the namespace narrows the menu to Skills: ${labels}`)
  assert.ok(offered[0].includes("/skill:commit") && offered[1].includes("/skill:deploy"), offered.join("|"))
  assert.equal(labels.some((label) => label.includes("/compact")), false, "no command row joins them")

  field.props["onUpdate:modelValue"]("/skill:dep")
  await flush()
  const deploy = all(root).find((element) => element.type === "button" && text(element).includes("/skill:deploy"))
  click(deploy)
  await flush()
  assert.deepEqual(submissions, [], "filling is not sending")

  const sent = []
  const typed = mount(t, "main", [], ComposerPanel, {}, { skills, send: async (message) => { sent.push(message) } })
  const typedField = all(typed.root).find((element) => element.type === "textarea")
  typedField.props.onFocus()
  typedField.props["onUpdate:modelValue"]("/skill:deploy ")
  await flush()
  typedField.props.onKeydown({ key: "Enter", preventDefault() {}, stopPropagation() {} })
  await flush()
  assert.deepEqual(sent, ["/skill:deploy "], "a /skill: draft is a message, never a command")
})

test("the input draws each /skill: token as one chip, typed or filled", async (t) => {
  const chips = (root) => all(root)
    .filter((element) => element.props.class === "skill-chip")
    .map(text)
  const skills = [{ name: "commit", description: "Writes commit messages" }]

  const typed = mount(t, "main", [], ComposerPanel, {}, { skills })
  const field = all(typed.root).find((element) => element.type === "textarea")
  field.props.onFocus()
  field.props["onUpdate:modelValue"]("please /skill:commit it")
  await flush()
  assert.deepEqual(chips(typed.root), ["/skill:commit"], "a typed token is one chip, and the words around it are not")

  const filled = mount(t, "main", [], ComposerPanel, {}, { skills })
  const filledField = all(filled.root).find((element) => element.type === "textarea")
  filledField.props.onFocus()
  filledField.props["onUpdate:modelValue"]("/skill:")
  await flush()
  const row = all(filled.root).find((element) => element.type === "button" && text(element).includes("/skill:commit"))
  click(row)
  await flush()
  assert.deepEqual(chips(filled.root), ["/skill:commit"], "a filled token is the same chip before it is sent")
})

test("a user message draws each /skill: token as one chip", async (t) => {
  const { root } = mount(t, "main", [], TimelineUserMessage, {}, { text: "please /skill:commit it", at: 0, quotable: false, branchable: false })
  const chips = all(root).filter((element) => element.props.class === "skill-chip").map(text)
  assert.deepEqual(chips, ["/skill:commit"], "the token is one chip and the words around it are not")
  assert.ok(text(root).includes("please /skill:commit it"), "the chip keeps the typed text")
})

test("the chip layer follows the field's width when the field resizes", async (t) => {
  const skills = [{ name: "commit", description: "Writes commit messages" }]
  globalThis.chatInputObservers = []
  const { root } = mount(t, "main", [], ComposerPanel, {}, { skills })
  const typed = all(root).find((element) => element.type === "textarea")
  typed.props.onFocus()
  typed.props["onUpdate:modelValue"]("please /skill:commit it")
  await flush()
  const body = all(root).find((element) => element.props.class === "composer-highlight-body")
  assert(body, "the chip layer draws the draft behind the field")
  assert.equal(globalThis.chatInputObservers.length, 1, "the composer watches the field, so a resize that no input caused still moves the layer")
  const observer = globalThis.chatInputObservers[0]
  const watched = observer.observed[0]
  assert.equal(watched.props.class, "composer-input", "it watches the field and nothing else")
  watched.clientWidth = 240
  observer.callback()
  assert.equal(body.style.width, "240px", "a width the draft did not cause moves the layer")
  watched.clientWidth = 180
  observer.callback()
  assert.equal(body.style.width, "180px", "every later width follows too")
  watched.scrollTop = 12
  watched.props.onScroll()
  assert.equal(body.style.transform, "translateY(-12px)", "the scroll path still carries the offset")
})

/** A stored image as the wire holds it: a reference to a Session file. */
const storedImage = (fileId) => ({ type: "image", source: { type: "file", mediaType: "image/png", fileId } })
/**
 * A held message as `session.input.pending` and `input/message` deliver it: its
 * own blocks in the order they were written, a string standing for a text run.
 */
const heldInput = (id, ...blocks) => {
  const content = blocks.map((block) => (typeof block === "string" ? { type: "text", text: block } : block))
  return { id, text: content.flatMap((block) => (block.type === "text" ? [block.text] : [])).join("\n"), skills: [], content, user: { content } }
}

/**
 * The composer alone, wired to recordings: `sent` takes every submit, `stop`
 * answers the stop, and `withdraw` answers each withdrawal, so a test drives
 * the gestures the chat page's own callbacks drive.
 */
function soloComposer(t, props = {}, stores = {}, calls = {}) {
  const sent = [], stopped = [], withdrawn = []
  const events = calls.events ?? []
  const mounted = mount(t, "main", [], ComposerPanel, stores, {
    running: true,
    attachments: true,
    send: async (value, attachments, mode) => { events.push("submit"); sent.push({ value, attachments, mode }) },
    stop: async () => {
      events.push("stop")
      stopped.push(true)
      if (calls.stopFails) throw new Error("the run would not stop")
      await calls.stopGate?.promise
    },
    withdrawHeld: async (item) => {
      events.push(`withdraw-start:${item.id}`)
      withdrawn.push(item.id)
      await calls.withdrawGate?.promise
      events.push(`withdraw-reply:${item.id}`)
      return calls.adopted?.includes(item.id) ? "notHeld" : "deleted"
    },
    ...props,
  })
  const field = all(mounted.root).find((element) => element.type === "textarea")
  field.props.onFocus()
  const enter = (modifiers = {}) => field.props.onKeydown({ key: "Enter", preventDefault() {}, stopPropagation() {}, ...modifiers })
  const type = async (value) => { field.props["onUpdate:modelValue"](value); await flush() }
  return { ...mounted, field, enter, type, sent, stopped, withdrawn }
}

for (const [label, sendMode, expected, alternate] of [
  ["steer by default", "steer", "steer", "queue"],
  ["a held default queues", "queue", "queue", "steer"],
]) {
  test(`during a run ${label}: Enter uses it and the other action does the opposite`, async (t) => {
    const { type, enter, root, sent } = soloComposer(t, {}, {
      composer: reactive({ sendMode, alternate() { return sendMode === "steer" ? "queue" : "steer" } }),
    })
    await type("follow up")
    enter()
    await flush()
    assert.deepEqual(sent.map((submission) => submission.mode), [expected], "Enter sends in this browser's mode")
    assert.match(text(root), /composer\.(sentIntoRun|heldForNextTurn)/, "the composer says where the draft went")

    await type("and another")
    await click(button(root, alternate === "steer" ? "composer.steerAlternate" : "composer.queueAlternate"))
    await flush()
    assert.deepEqual(sent.map((submission) => submission.mode), [expected, alternate], "the other action sends in the other mode")
  })
}

test("an idle run always submits as queue, whatever this browser's default is", async (t) => {
  for (const sendMode of ["steer", "queue"]) {
    const { type, enter, root, sent } = soloComposer(t, { running: false }, {
      composer: reactive({ sendMode, alternate() { return "queue" } }),
    })
    await type("start it")
    enter()
    await flush()
    assert.deepEqual(sent.map((submission) => submission.mode), ["queue"], `idle with a ${sendMode} default`)
    assert.equal(button(root, "composer.steerAlternate"), undefined, "no run, no alternate action")
    assert.equal(button(root, "composer.queueAlternate"), undefined)
    assert.equal(button(root, "composer.sendNowHint"), undefined)
  }
})

test("interrupt and send now withdraws every held message oldest first, stops, then submits once", async (t) => {
  let release
  const stopGate = { promise: new Promise((resolve) => { release = resolve }) }
  const held = [heldInput("first", "one"), heldInput("second", "two")]
  const { type, enter, root, sent, stopped, withdrawn } = soloComposer(t, { held }, {}, { stopGate })
  await type("and then")

  const gesture = click(button(root, "composer.sendNowHint"))
  await flush()
  assert.deepEqual(withdrawn, ["first", "second"], "the withdrawals reach the session before the stop finishes")
  assert.deepEqual(stopped, [true], "one stop follows the withdrawals")
  assert.deepEqual(sent, [], "nothing is submitted while the stop settles")

  enter({ ctrlKey: true })
  enter()
  await flush()
  assert.deepEqual(stopped, [true], "a second trigger asks for no second stop")
  assert.deepEqual(sent, [], "and submits nothing while the first one settles")

  release()
  await gesture
  await flush()
  assert.deepEqual(sent.map((submission) => [submission.value, submission.mode]), [["one\ntwo\nand then", "queue"]], "one submit, after the stop's reply, with the recalled messages ahead of the draft")
})

test("an empty draft held through a delayed withdrawal stops once, after every reply", async (t) => {
  let release
  const withdrawGate = { promise: new Promise((resolve) => { release = resolve }) }
  const events = []
  const held = [heldInput("first", "one"), heldInput("second", "two")]
  const run = soloComposer(t, { held }, {}, { withdrawGate, events })

  const gesture = click(button(run.root, "composer.sendNowHint"))
  await flush()
  assert.deepEqual(events, ["withdraw-start:first"], "the withdrawal is out and unanswered")

  const stopButton = button(run.root, "composer.stopResponse")
  assert.equal(stopButton.props.disabled, true, "the ordinary Stop waits for the gesture that is already stopping the run")
  await click(stopButton)
  await flush()
  assert.deepEqual(events, ["withdraw-start:first"], "a Stop click while the gesture runs sends no stop of its own")

  release()
  await gesture
  await flush()
  assert.deepEqual(events, ["withdraw-start:first", "withdraw-reply:first", "withdraw-start:second", "withdraw-reply:second", "stop", "submit"], "every withdrawal replies before the one stop, and the submit follows it")
  assert.equal(run.stopped.length, 1, "one stop for the gesture")
  assert.deepEqual(run.sent.map((submission) => submission.value), ["one\ntwo"], "the recalled messages are submitted once")
})

/** A page prompt that takes the composer's place, shown and cleared while a stop is out. */
const promptTakes = {
  question: (view, branchId) => ({
    take: async () => { view.questions = [question(branchId, "while-stopping")]; await flush() },
    clear: async () => { view.questions = []; await flush() },
  }),
  approval: (view, branchId) => ({
    take: async () => {
      view.approvals = [{ sessionId: "session", branchId, requestId: "while-stopping", call: { name: "exec", input: { command: "ls" } }, reason: "run ls" }]
      await flush()
    },
    clear: async () => { view.approvals = []; await flush() },
  }),
}

for (const [branchId, kind] of [["main", "question"], ["main", "approval"], ["worker", "question"], ["worker", "approval"]]) {
  test(`${kind} on ${branchId}: a prompt that takes the composer's place keeps a withdrawn draft through a failed stop`, async (t) => {
    const picture = storedImage("stored-before")
    const held = heldInput("first", "before ", picture, " after")
    const { root, view, submissions } = mount(t, branchId)
    const chat = globalThis.chatInputStores.chat
    let releaseWithdrawal, failStop
    const withdrawal = new Promise((resolve) => { releaseWithdrawal = resolve })
    chat.withdrawInput = async (sessionId, branch, inputId) => {
      globalThis.chatInputWithdrawals.push({ sessionId, branch, inputId })
      await withdrawal
      if (branchId === "main") view.queued = []
      else view.branches.worker.queued = []
      return { status: "deleted" }
    }
    chat.stop = async (sessionId, branch) => {
      globalThis.chatInputStops.push({ sessionId, branch })
      return new Promise((_, reject) => { failStop = () => reject(new Error("the run would not stop")) })
    }
    view.methods = [...view.methods, "session.input.withdraw"]
    if (branchId === "main") view.queued = [held]
    else view.branches.worker.queued = [held]
    await flush()
    const field = all(root).find((element) => element.type === "textarea")
    field.props.onFocus()
    field.props["onUpdate:modelValue"]("draft")
    await flush()
    const shown = field.focuses

    const gesture = click(button(root, "composer.sendNowHint"))
    await flush()
    assert.equal(globalThis.chatInputWithdrawals.length, 1, "the gesture withdraws the held message once")
    releaseWithdrawal()
    await flush()
    assert.equal(globalThis.chatInputStops.length, 1, "the stop is out after the withdrawal's reply")

    const prompts = promptTakes[kind](view, branchId)
    await prompts.take()
    assert.equal(all(root).some((element) => element.type === "textarea"), false, `the ${kind} takes the composer's place`)
    assert.equal(submissions.length, 0, "nothing is submitted while the stop is out")
    const hidden = field.focuses
    assert.equal(hidden, shown, `the ${kind} takes the caret and the composer it holds no place for does not`)

    failStop()
    await gesture
    await flush()
    assert.equal(globalThis.chatInputWithdrawals.length, 1, "the withdrawal that went is not repeated")
    assert.equal(submissions.length, 0, "a stop that fails sends nothing")

    await prompts.clear()
    const back = all(root).find((element) => element.type === "textarea")
    assert.notEqual(back, undefined, "the composer comes back when the prompt clears")
    assert.equal(back.value, "before  after\ndraft", "the recalled message and the typed draft are still there")
    assert.equal(back.focuses, hidden + 1, "it takes the caret once on the way back")
    assert.equal(all(root).filter((element) => element.type === "img").length, 1, "the recalled stored image is still in the draft")
    assert.match(text(root), /composer\.sendNowFailed/, "the reason is reported where the draft came back")

    // The message left the queue when it was recalled, so a send that fails keeps
    // the whole draft and the next retry is the one that delivers it.
    chat.stop = async () => { globalThis.chatInputStops.push({ sessionId: "session", branch: branchId }) }
    chat.submit = async () => { throw new Error("the connection dropped") }
    await click(button(root, "composer.sendNowHint"))
    await flush()
    assert.equal(globalThis.chatInputStops.length, 2, "the retry asks for the stop again")
    assert.match(text(root), /the connection dropped/, "a send that fails says why")
    assert.equal(back.value, "before  after\ndraft", "and keeps the whole draft")
    assert.equal(submissions.length, 0, "while nothing reaches the session")

    chat.submit = async (sessionId, branch, value, attachments, mode) => { submissions.push({ sessionId, branch, value, attachments, mode }) }
    await click(button(root, "composer.sendNowHint"))
    await flush()
    assert.equal(globalThis.chatInputWithdrawals.length, 1, "the retry recalls nothing a second time")
    assert.equal(globalThis.chatInputStops.length, 3, "and asks for the stop again")
    assert.equal(submissions.length, 1, "the retry sends once")
    assert.equal(submissions[0].value, "before  after\ndraft", "the recalled words keep their order ahead of the typed draft")
    assert.deepEqual(submissions[0].attachments.restored, [{ at: 7, image: picture }], "the recalled image keeps its place between the words")
    assert.equal(submissions[0].mode, "queue")

    await prompts.take()
    await prompts.clear()
    assert.equal(all(root).find((element) => element.type === "textarea").value, "", "the accepted draft is gone from the composer")
    assert.equal(all(root).filter((element) => element.type === "img").length, 0, "and its image with it")
  })
}

test("an ended line shows no composer, so nothing can be sent from it", async (t) => {
  const { root, view } = mount(t, "worker")
  view.lines[0].state = { kind: "ended" }
  await flush()
  assert.equal(all(root).find((element) => element.type === "textarea"), undefined)
  assert.match(text(root), /lines\.readOnly/)
})

test("switching the shown line leaves one composer's draft out of the other's", async (t) => {
  const view = reactive({
    status: "ready", rows: [], running: true, stopping: false, sessionRunning: true,
    approvals: [], questions: [], methods: ["attachment.put", "session.files.search", "session.input.withdraw"], queued: [],
    lines: [{ id: "worker", purpose: "Worker", parentId: "main", state: { kind: "working" } }, { id: "other", purpose: "Other", parentId: "main", state: { kind: "working" } }],
    branches: { worker: { status: "ready", rows: [], running: true }, other: { status: "ready", rows: [], running: true } },
  })
  const ui = reactive({
    lineTabs: { session: ["worker", "other"] },
    activeLineTab: { session: "worker" },
    detailExpanded: false, detailFocused: true, extension: undefined,
    openLines() {}, openLine() {}, closeLineTab() {}, closeDetail() {},
    activateLine(sessionId, branchId) { this.activeLineTab = { ...this.activeLineTab, [sessionId]: branchId } },
    approvalKeyboard: () => false,
  })
  const { default: LinesPanel } = await import("../src/features/lines/LinesPanel.vue")
  const { root } = mount(t, "worker", [], LinesPanel, { ui }, { sessionId: "session" }, view)
  await flush()
  const field = all(root).find((element) => element.type === "textarea")
  field.props.onFocus()
  field.props["onUpdate:modelValue"]("line draft")
  await flush()

  ui.activateLine("session", "other")
  await flush()
  assert.equal(all(root).includes(field), false, "the other line shows its own composer, not this one")
  const other = all(root).find((element) => element.type === "textarea")
  assert.equal(other?.value ?? "", "", "the other line's composer starts empty")
  assert.equal(all(root).filter((element) => element.type === "img").length, 0, "and does not carry the recalled image")

  other.props.onFocus()
  other.props["onUpdate:modelValue"]("other draft")
  await flush()
  ui.activateLine("session", "worker")
  await flush()
  const back = all(root).find((element) => element.type === "textarea")
  assert.equal(/other draft/.test(back?.value ?? ""), false, "the worker's composer shows no part of the other line's draft")
})

test("a prompt that takes the composer's place keeps the draft typed before it", async (t) => {
  for (const [branchId, kind] of [["main", "question"], ["worker", "approval"]]) {
    const { root, view } = mount(t, branchId)
    await flush()
    const field = all(root).find((element) => element.type === "textarea")
    field.props.onFocus()
    field.props["onUpdate:modelValue"]("half a thought")
    await flush()

    const prompts = promptTakes[kind](view, branchId)
    await prompts.take()
    assert.equal(all(root).some((element) => element.type === "textarea"), false, `the ${kind} takes the composer's place`)
    await prompts.clear()

    const back = all(root).find((element) => element.type === "textarea")
    assert.equal(back.value, "half a thought", `the draft typed before the ${kind} is still there`)
    const before = back.focuses
    await prompts.take()
    await prompts.clear()
    assert.equal(back.focuses, before + 1, `the composer the ${kind} displaces takes its caret back once`)
  }
})

test("a prompt that takes the composer's place keeps it hidden through a stop that succeeds", async (t) => {
  const picture = storedImage("stored-before")
  const held = heldInput("first", "before ", picture, " after")
  const { root, view, submissions } = mount(t, "main")
  const chat = globalThis.chatInputStores.chat
  let releaseWithdrawal, settleStop
  const withdrawal = new Promise((resolve) => { releaseWithdrawal = resolve })
  chat.withdrawInput = async (sessionId, branch, inputId) => {
    globalThis.chatInputWithdrawals.push({ sessionId, branch, inputId })
    await withdrawal
    view.queued = []
    return { status: "deleted" }
  }
  chat.stop = async (sessionId, branch) => {
    globalThis.chatInputStops.push({ sessionId, branch })
    return new Promise((resolve) => { settleStop = resolve })
  }
  view.methods = [...view.methods, "session.input.withdraw"]
  view.queued = [held]
  await flush()
  const field = all(root).find((element) => element.type === "textarea")
  field.props.onFocus()
  field.props["onUpdate:modelValue"]("draft")
  await flush()

  const gesture = click(button(root, "composer.sendNowHint"))
  await flush()
  assert.equal(globalThis.chatInputWithdrawals.length, 1, "the gesture withdraws the held message")
  releaseWithdrawal()
  await flush()
  assert.equal(globalThis.chatInputStops.length, 1, "the stop is out after the withdrawal's reply")

  view.questions = [question("main", "while-stopping")]
  await flush()
  assert.equal(all(root).some((element) => element.type === "textarea"), false, "the question takes the composer's place")

  settleStop()
  await gesture
  await flush()
  assert.equal(submissions.length, 1, "the draft goes out once the stopped run has settled")
  assert.equal(submissions[0].value, "before  after\ndraft", "with the recalled message ahead of the typed draft")
  assert.equal(all(root).some((element) => element.type === "textarea"), false, "and the composer stays hidden behind the question")

  view.questions = []
  await flush()
  const back = all(root).find((element) => element.type === "textarea")
  assert.equal(back.value, "", "the composer that returns shows nothing the accepted message took")
  assert.equal(all(root).filter((element) => element.type === "img").length, 0, "and no part of its image")
  assert.notEqual(back.style.height, "0px", "not the height it measured while it was hidden")
})

test("a composer the page keeps for a prompt is released with the page", async (t) => {
  const urls = []
  const realCreate = URL.createObjectURL, realRevoke = URL.revokeObjectURL
  URL.createObjectURL = (file) => { const url = `blob:${file.name}`; urls.push(url); return url }
  URL.revokeObjectURL = (url) => { urls.splice(urls.indexOf(url), 1) }
  t.after(() => { URL.createObjectURL = realCreate; URL.revokeObjectURL = realRevoke })
  const { root, view, unmount } = mount(t, "main")
  await flush()
  const field = all(root).find((element) => element.type === "textarea")
  field.props.onFocus()
  field.props.onPaste({ clipboardData: { files: [new File(["bytes"], "kept.png", { type: "image/png" })] }, preventDefault() {} })
  await flush()
  assert.deepEqual(urls, ["blob:kept.png"], "the draft holds its preview while the composer is mounted")

  view.questions = [question("main", "while-hidden")]
  await flush()
  assert.deepEqual(urls, ["blob:kept.png"], "a prompt that takes its place keeps the draft, preview and all")
  view.questions = []
  await flush()
  assert.equal(all(root).find((element) => element.type === "textarea").value, "")
  assert.equal(all(root).filter((element) => element.type === "img").length, 1, "the preview is back with the composer")
  assert.deepEqual(urls, ["blob:kept.png"], "and was never released on the way")

  view.questions = [question("main", "while-gone")]
  await flush()
  unmount()
  assert.deepEqual(urls, [], "the page that goes releases the composer it kept while a prompt held its place")
})

test("a stop that fails keeps the whole draft in the composer and sends nothing", async (t) => {
  const held = [heldInput("first", "one"), heldInput("second", "two")]
  const { type, root, field, sent, withdrawn } = soloComposer(t, { held }, {}, { stopFails: true })
  await type("and then")
  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.match(text(root), /composer\.sendNowFailed/, "the reason is reported")
  assert.equal(field.value, "one\ntwo\nand then", "every recalled message and the typed draft stay")
  assert.deepEqual(sent, [], "nothing was sent")
  assert.deepEqual(withdrawn, ["first", "second"], "the withdrawals that already went are not repeated")
})

test("a message the run already adopted is not recalled or sent again", async (t) => {
  const held = [heldInput("first", "one"), heldInput("second", "two")]
  const { type, root, sent } = soloComposer(t, { held }, {}, { adopted: ["first"] })
  await type("and then")
  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.deepEqual(sent.map((submission) => submission.value), ["two\nand then"], "only the message that was still held comes back")
})

test("recalled messages keep their stored images beside the draft's own attachments", async (t) => {
  const picture = storedImage("stored-1")
  const { field, type, root, sent } = soloComposer(t, { held: [heldInput("first", "look", picture)] })
  const local = new File(["bytes"], "draft.png", { type: "image/png" })
  field.props.onPaste({ clipboardData: { files: [local] }, preventDefault() {} })
  await type("and then")
  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.equal(sent.length, 1)
  assert.equal(sent[0].value, "look\nand then")
  assert.deepEqual(sent[0].attachments.images.map((image) => image.name), ["draft.png"], "the draft's own file is still uploaded")
  assert.deepEqual(sent[0].attachments.restored, [{ at: 4, image: picture }], "the recalled message's stored image is sent unchanged, where it stood after 'look'")
})

test("recalled messages keep their images between their own words through stop and the final submit", async (t) => {
  const before = storedImage("stored-before"), after = storedImage("stored-after")
  const held = [heldInput("first", "look ", before), heldInput("second", "here ", after, " end")]
  const { type, root, sent } = soloComposer(t, { held })
  await type("draft")
  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.equal(sent.length, 1, "the draft is submitted once")
  assert.equal(sent[0].value, "look \nhere  end\ndraft", "the recalled words keep queue order, each image between them taking no room in the field")
  assert.deepEqual(sent[0].attachments.restored.map(({ at, image }) => [at, image.source.fileId]), [[5, "stored-before"], [11, "stored-after"]], "every image keeps the draft offset it stood at")
})

test("a recalled mixed message reaches session.submit with its images where they stood", async (t) => {
  setActivePinia(createPinia())
  const chat = useChatStore()
  t.after(() => chat.$dispose())
  const calls = []
  const connection = { supports: () => false, client: { async call(method, params, read) { calls.push({ method, params }); return read({}) } } }
  const before = storedImage("stored-before")
  const held = [heldInput("mixed", "before ", before, " after"), heldInput("second", "next message")]
  const send = (value, attachments, mode) => chat.submit("session", "main", value, attachments, mode)
  const { type, root } = soloComposer(t, { held, send }, { connection })
  await type("draft")
  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.deepEqual(calls.map((call) => call.method), ["session.submit"], "the stop and the recall reach the session through their own calls")
  assert.deepEqual(calls[0].params.user.content, [
    { type: "text", text: "before " },
    before,
    { type: "text", text: " after\nnext message\ndraft" },
  ], "each recalled image stays between the words it stood between, ahead of the next message and the draft")
})

test("a recalled image keeps the place an edit moved it to after a failed stop", async (t) => {
  setActivePinia(createPinia())
  const chat = useChatStore()
  t.after(() => chat.$dispose())
  const calls = []
  const connection = { supports: () => false, client: { async call(method, params, read) { calls.push({ method, params }); return read({}) } } }
  const picture = storedImage("stored-1")
  const send = (value, attachments, mode) => chat.submit("session", "main", value, attachments, mode)
  let stops = 0
  const stop = async () => {
    stops++
    if (stops === 1) throw new Error("the run would not stop")
  }
  const { type, root, field } = soloComposer(t, { held: [heldInput("first", "see ", picture, " here")], send, stop }, { connection })
  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.equal(field.value, "see  here", "the recalled words stay, the image keeping its own offset between them")
  assert.match(text(root), /composer\.sendNowFailed/, "the failed stop is reported")
  assert.deepEqual(calls, [], "a stop that fails sends nothing")

  await type("look: see  here")
  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.equal(stops, 2, "the retry asks for the stop again")
  assert.deepEqual(calls.map((call) => call.method), ["session.submit"], "the retry sends once")
  assert.deepEqual(calls[0].params.user.content, [{ type: "text", text: "look: see " }, picture, { type: "text", text: " here" }], "the image moved with the words around it")
})

test("a draft typed while a recalled image is being accepted stays out of the message already sent", async (t) => {
  setActivePinia(createPinia())
  const chat = useChatStore()
  t.after(() => chat.$dispose())
  const calls = []
  const connection = { supports: () => false, client: { async call(method, params, read) { calls.push({ method, params }); return read({}) } } }
  const picture = storedImage("stored-1")
  let release
  const gate = new Promise((resolve) => { release = resolve })
  const send = async (value, attachments, mode) => {
    await chat.submit("session", "main", value, attachments, mode)
    if (calls.length === 1) await gate
  }
  const { type, root, field } = soloComposer(t, { held: [heldInput("first", "before", picture, "after")], send, stop: async () => {} }, { connection })
  const gesture = click(button(root, "composer.sendNowHint"))
  await flush()
  assert.equal(calls.length, 1, "the recalled message is out and unanswered")
  assert.deepEqual(calls[0].params.user.content, [{ type: "text", text: "before" }, picture, { type: "text", text: "after" }], "the recalled image keeps the place it stood at")

  await type("next draft")
  release()
  await gesture
  await flush()
  await click(button(root, "composer.send"))
  await flush()
  assert.equal(calls.length, 2, "what was typed during the send goes out on its own")
  assert.equal(calls[1].params.user, undefined, "the image the accepted message took is not sent a second time")
  assert.equal(calls[1].params.text, "next draft", "only what was typed after the first message goes out")
  assert.equal(field.value, "", "the line clears once the draft is accepted")
})

test("a message recalled after a failed stop moves the image already in the draft", async (t) => {
  setActivePinia(createPinia())
  const chat = useChatStore()
  t.after(() => chat.$dispose())
  const calls = []
  const connection = { supports: () => false, client: { async call(method, params, read) { calls.push({ method, params }); return read({}) } } }
  const first = storedImage("stored-first"), second = storedImage("stored-second")
  const send = (value, attachments, mode) => chat.submit("session", "main", value, attachments, mode)
  let stops = 0
  const stop = async () => {
    stops++
    if (stops === 1) throw new Error("the run would not stop")
  }
  const held = reactive([heldInput("first", "see ", first, " here")])
  const withdrawHeld = async (item) => {
    held.splice(held.indexOf(item), 1)
    return "deleted"
  }
  const { type, root, field } = soloComposer(t, { held, withdrawHeld, send, stop }, { connection })
  await type("draft")
  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.equal(field.value, "see  here\ndraft", "the failed stop keeps the recalled message ahead of the draft")

  held.push(heldInput("second", "and ", second, " now"))
  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.equal(stops, 2, "the retry asks for the stop again")
  assert.deepEqual(calls.map((call) => call.method), ["session.submit"], "the retry sends once")
  assert.deepEqual(calls[0].params.user.content, [
    { type: "text", text: "and " }, second,
    { type: "text", text: " now\nsee " }, first,
    { type: "text", text: " here\ndraft" },
  ], "the image the first recall placed moved with the text it stood between instead of staying behind the new message")
  assert.equal(field.value, "", "the whole draft is accepted once")
})

test("a message recalled after a failed stop goes ahead of an image at the start of the draft", async (t) => {
  setActivePinia(createPinia())
  const chat = useChatStore()
  t.after(() => chat.$dispose())
  const calls = []
  const connection = { supports: () => false, client: { async call(method, params, read) { calls.push({ method, params }); return read({}) } } }
  const picture = storedImage("stored-1")
  const send = (value, attachments, mode) => chat.submit("session", "main", value, attachments, mode)
  let stops = 0
  const stop = async () => {
    stops++
    if (stops === 1) throw new Error("the run would not stop")
  }
  const held = reactive([heldInput("first", picture, "old words")])
  const withdrawHeld = async (item) => {
    held.splice(held.indexOf(item), 1)
    return "deleted"
  }
  const { root } = soloComposer(t, { held, withdrawHeld, send, stop }, { connection })
  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.deepEqual(calls, [], "a stop that fails sends nothing")

  held.push(heldInput("second", "new words"))
  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.equal(stops, 2, "the retry asks for the stop again")
  assert.deepEqual(calls[0].params.user.content, [
    { type: "text", text: "new words\n" }, picture,
    { type: "text", text: "old words" },
  ], "the image the draft starts with stays with its own words, behind the whole new message")
})

test("two image-only messages recalled in one gesture keep the queue order", async (t) => {
  setActivePinia(createPinia())
  const chat = useChatStore()
  t.after(() => chat.$dispose())
  const calls = []
  const connection = { supports: () => false, client: { async call(method, params, read) { calls.push({ method, params }); return read({}) } } }
  const send = (value, attachments, mode) => chat.submit("session", "main", value, attachments, mode)
  const pictures = [storedImage("stored-first"), storedImage("stored-second")]
  const held = [heldInput("first", pictures[0]), heldInput("second", pictures[1])]
  const { root } = soloComposer(t, { held, send }, { connection })
  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.deepEqual(calls[0].params.user.content, pictures, "two image-only messages keep the order they were held in")
})

test("an image-only message stays ahead of the message recalled after it", async (t) => {
  setActivePinia(createPinia())
  const chat = useChatStore()
  t.after(() => chat.$dispose())
  const calls = []
  const connection = { supports: () => false, client: { async call(method, params, read) { calls.push({ method, params }); return read({}) } } }
  const send = (value, attachments, mode) => chat.submit("session", "main", value, attachments, mode)
  const picture = storedImage("stored-first")
  const held = [heldInput("image", picture), heldInput("text", "more")]
  const { root } = soloComposer(t, { held, send }, { connection })
  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.deepEqual(calls[0].params.user.content, [picture, { type: "text", text: "more" }], "an image-only message stays ahead of the message recalled after it")
})

test("an image-only message recalled after a failed stop goes ahead of the image-only draft", async (t) => {
  setActivePinia(createPinia())
  const chat = useChatStore()
  t.after(() => chat.$dispose())
  const calls = []
  const connection = { supports: () => false, client: { async call(method, params, read) { calls.push({ method, params }); return read({}) } } }
  const old = storedImage("stored-old"), next = storedImage("stored-next")
  const send = (value, attachments, mode) => chat.submit("session", "main", value, attachments, mode)
  let stops = 0
  const stop = async () => {
    stops++
    if (stops === 1) throw new Error("the run would not stop")
  }
  const held = reactive([heldInput("first", old)])
  const withdrawHeld = async (item) => {
    held.splice(held.indexOf(item), 1)
    return "deleted"
  }
  const { root, field } = soloComposer(t, { held, withdrawHeld, send, stop }, { connection })
  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.deepEqual(calls, [], "a stop that fails sends nothing")
  assert.equal(field.value, "", "an image-only message leaves no text in the draft")

  held.push(heldInput("second", next))
  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.equal(stops, 2, "the retry asks for the stop again")
  assert.deepEqual(calls[0].params.user.content, [next, old], "the new message goes ahead of the image-only draft")
})

test("a recalled image still splits the message when an inserted @ file is linked around it", async (t) => {
  setActivePinia(createPinia())
  const chat = useChatStore()
  t.after(() => chat.$dispose())
  const calls = []
  const connection = { supports: () => false, client: { async call(method, params, read) { calls.push({ method, params }); return read({}) } } }
  const picture = storedImage("stored-1")
  const send = (value, attachments, mode) => chat.submit("session", "main", value, attachments, mode)
  let stops = 0
  const stop = async () => {
    stops++
    if (stops === 1) throw new Error("the run would not stop")
  }
  const { root, field } = soloComposer(t, { held: [heldInput("first", "see ", picture, " here")], send, stop, searchFiles: async () => [{ relativePath: "report.md", absolutePath: "/repo/report.md", dir: false }] }, { connection })
  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.deepEqual(calls, [], "the failed stop kept the recalled draft instead of sending it")

  const draft = "see  here @report"
  field.value = draft
  field.selectionStart = draft.length
  field.props["onUpdate:modelValue"](draft)
  await flush()
  field.props.onInput()
  await new Promise((resolve) => setTimeout(resolve, 100))
  await flush()
  const candidate = all(root).find((element) => element.props.role === "option" && text(element) === "report.md")
  assert(candidate)
  await click(candidate)
  await flush()
  assert.equal(field.value, "see  here @report.md ", "the file is inserted with its token")

  await click(button(root, "composer.sendNowHint"))
  await flush()
  assert.deepEqual(calls.map((call) => call.method), ["session.submit"], "the retry sends once")
  assert.deepEqual(calls[0].params.user.content, [{ type: "text", text: "see " }, picture, { type: "text", text: " here [@report.md](/repo/report.md) " }], "the link is the text after the image, which keeps its place between the words")
  assert.equal(field.value, "", "the line clears once the draft is accepted")
})

test("Ctrl+Enter while the run is idle submits as Enter does, and a picker keeps it as Enter", async (t) => {
  const idle = soloComposer(t, { running: false })
  await idle.type("start it")
  idle.enter({ ctrlKey: true })
  await flush()
  assert.deepEqual(idle.sent.map((submission) => submission.value), ["start it"], "idle, the key sends the draft")
  assert.deepEqual(idle.stopped, [], "and stops nothing")

  const picker = soloComposer(t, { createBranch: async () => {} })
  await picker.type("/branch")
  picker.enter({ ctrlKey: true })
  await flush()
  assert.deepEqual(picker.stopped, [], "a picker takes the key as Enter itself, so no run is stopped")
})

test("a send-now with an empty draft and nothing held does nothing, and one with no held message still stops", async (t) => {
  const empty = soloComposer(t, { held: [] })
  await click(button(empty.root, "composer.sendNowHint"))
  await flush()
  assert.deepEqual([empty.stopped, empty.sent], [[], []], "an empty draft with nothing held is a no-op")

  const typed = soloComposer(t, { held: [] })
  await typed.type("and then")
  await click(button(typed.root, "composer.sendNowHint"))
  await flush()
  assert.deepEqual(typed.stopped, [true], "a typed draft stops the run")
  assert.deepEqual(typed.sent.map((submission) => [submission.value, submission.mode]), [["and then", "queue"]], "and is sent once it settles")
})

test("a composer with no withdrawal action sends now without recalling, and reports what it cannot do", async (t) => {
  const held = [heldInput("first", "one")]
  const { type, root, sent, stopped } = soloComposer(t, { held, withdrawHeld: undefined })
  await type("and then")
  assert.equal(button(root, "composer.sendNowHint").props.disabled, true, "a message it cannot recall keeps the action unavailable")
  const typed = soloComposer(t, { held: [], withdrawHeld: undefined })
  await typed.type("and then")
  await click(button(typed.root, "composer.sendNowHint"))
  await flush()
  assert.deepEqual(typed.stopped, [true], "with nothing held it is the same gesture")
  assert.deepEqual(typed.sent.map((submission) => submission.value), ["and then"])
  const idle = soloComposer(t, { running: false, held: [] })
  await idle.type("start it")
  await idle.enter({ ctrlKey: true })
  await flush()
  assert.deepEqual([idle.stopped, idle.sent.map((submission) => submission.value)], [[], ["start it"]], "idle it submits and stops nothing")
})

test("the send-mode control on the Appearance page changes which action steers", async (t) => {
  globalThis.chatInputStores = {
    chat: { views: {}, open() {}, ensureLine() {} },
    connection: { supports: () => false },
    models: { available: false, load: async () => {} },
    sessions: { byId: new Map(), summaries: [] },
    toast: { push() {} },
    ui: { shellMode: "wide" },
    appearance: reactive({ theme: "system", fontSize: 14, language: "", error: "", selectLanguage: async () => {} }),
    composer: reactive({ sendMode: "steer", error: "", alternate() { return this.sendMode === "steer" ? "queue" : "steer" } }),
  }
  const root = node("root")
  const app = renderer.createApp(AppearanceSettings)
  app.use(nuxtUiTestPlugin)
  app.mount(root)
  t.after(() => app.unmount())
  await flush()

  const modeButton = (label) => all(root).find((element) => element.type === "button" && text(element) === label)
  assert.equal(modeButton("Steer into the run").props["aria-pressed"], true, "steer is the default the page draws")
  await click(modeButton("Hold for the next turn"))
  await flush()
  assert.equal(globalThis.chatInputStores.composer.sendMode, "queue", "the page writes this browser's choice")
  assert.equal(modeButton("Hold for the next turn").props["aria-pressed"], true)
  assert.equal(modeButton("Steer into the run").props["aria-pressed"], false)
})
