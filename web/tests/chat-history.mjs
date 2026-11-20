import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { registerHooks, stripTypeScriptTypes } from "node:module"
import test from "node:test"
import { createPinia, setActivePinia } from "pinia"

registerHooks({
  resolve(specifier, context, next) {
    if (context.parentURL?.endsWith("/stores/chat.ts") && ["./connection", "./sessions", "./ui"].includes(specifier)) {
      const store = specifier.slice(2)
      return { url: `data:text/javascript,export const use${store[0].toUpperCase()}${store.slice(1)}Store=()=>globalThis.historyStores.${store}`, shortCircuit: true }
    }
    try { return next(specifier, context) } catch (error) {
      if (specifier.startsWith(".") && !specifier.endsWith(".ts")) return next(`${specifier}.ts`, context)
      throw error
    }
  },
  load(url, context, next) {
    if (url.endsWith(".ts")) return { format: "module", source: stripTypeScriptTypes(readFileSync(new URL(url), "utf8"), { mode: "transform" }), shortCircuit: true }
    return next(url, context)
  },
})
const { useChatStore } = await import("../src/stores/chat.ts")
const { RpcError, RpcClient, ConnectionLost } = await import("../src/rpc/client.ts")
const event = (sequence, content) => ({ id: `e${sequence}`, sequence, createAt: sequence, content })
const input = (sequence, text) => event(sequence, { kind: "input", text, content: [{ type: "text", text }], blocks: [] })
const page = (events, through, nextAfter = through, done = true) => ({ events, through, nextAfter, done })
const flush = () => new Promise(resolve => setTimeout(resolve, 25))
const deferred = () => { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no }); return { promise, resolve, reject } }

function session(t, history, reads = {}, display = true, transcript = false) {
  setActivePinia(createPinia())
  globalThis.historyStores = {
    connection: { supports: name => (!["session.history.display.tail", "session.history.display.transcript"].includes(name) || transcript) && name !== "session.permission.get" && (display || !["session.history.display", "session.history.detail"].includes(name)), client: { async call(method, params) {
      if (method === "session.input.pending" || method === "session.question.pending") {
        assert.deepEqual(Object.keys(params).filter(key => !["sessionId", "branchId", "through", "after"].includes(key)), [], "pending reads reject unknown wire fields")
      }
      if (reads[method]) return reads[method](params)
      if (method === "session.history" || method === "session.history.display") return history(params)
      if (method === "session.attach") return { providerId: "test", modelId: "test" }
      if (method === "session.methods") return ["session.input.pending", "session.question.pending", "session.turn.get", "session.goal.get"]
      if (method === "session.branches") return { branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 0, state: "open" }], defaultBranchId: "main" }
      if (method === "session.branch.get") return { ...params, parentBranchId: "main", branchPoint: 0, head: 0, state: "open" }
      if (method === "session.input.pending") return { ...params, inputs: [], next: null }
      if (method === "session.question.pending") return { ...params, asks: [], next: null }
      if (method === "session.turn.get") return { ...params, turn: undefined, output: undefined }
      return undefined
    } } },
    sessions: { byId: new Map(), setRunning() {}, refresh: async () => {}, forget() {} },
    ui: { resetSessionTabs() {}, followDefault() {} },
  }
  const chat = useChatStore()
  t.after(() => chat.$dispose())
  chat.open("session")
  return chat
}
const receive = (chat, branchId, event) => chat.receive({ method: "session.event", sessionId: "session", branchId, event })

test("the centre follows the Session's default branch and its commands name it", async t => {
  const seen = {}
  const policy = { preset: "workspace-write", allowedTools: [], disallowedTools: [] }
  const chat = session(t, params => {
    seen.history = params.branchId
    return page([input(1, "on the line")], 1)
  }, {
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: "worker" }),
    "session.goal.get": params => { seen.goal = params.branchId; return { branchId: params.branchId, through: 1, goal: null } },
    "session.permission.get": params => { seen.permission = params.branchId; return { version: 1, policy } },
    "session.context.compact": params => { seen.compact = params.branchId; return { status: "skipped", reason: "short" } },
    "session.context.usage": params => { seen.usage = params.branchId; return { contextWindow: 1000, outputTokens: 10, thresholdTokens: 800, estimatedInputTokens: 100, baselineSource: "estimated" } },
    "session.submit": params => { seen.submit = params.branchId; return { event: event(9, { kind: "other" }) } },
  })
  const supports = globalThis.historyStores.connection.supports
  globalThis.historyStores.connection.supports = name => name === "session.permission.get" || supports(name)
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "worker")
  assert.equal(seen.history, "worker", "the centre reads the default branch's history")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on the line"], "the shown transcript is the default branch's")
  assert.equal(seen.goal, "worker", "the goal in force is the default branch's")
  assert.equal(seen.permission, "worker", "the permission read is the default branch's")
  await chat.compact("session")
  assert.equal(seen.compact, "worker", "/compact acts on the centred line")
  await chat.usage("session")
  assert.equal(seen.usage, "worker", "/usage acts on the centred line")
  await chat.submit("session", chat.views.session.defaultBranchId, "hello")
  assert.equal(seen.submit, "worker", "a message from the centre reaches the default branch")
})

test("a move whose new centre cannot be read reports the failure instead of an empty conversation", async t => {
  let moved = false
  const chat = session(t, params => {
    if (moved && params.branchId === "worker") throw new Error("the line could not be read")
    return page([input(1, "on the root")], 1)
  }, {
    "session.branch_set_default": () => {
      moved = true
      return "worker"
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  await chat.setDefaultBranch("session", "worker")
  assert.equal(chat.views.session.defaultBranchId, "worker", "the move stands")
  assert.equal(chat.views.session.status, "failed", "the unreadable centre is reported, not shown empty")
  assert.equal(chat.views.session.error, "the line could not be read")
})

for (const display of [true, false]) test(`conversation windows negotiate ${display ? "display summaries" : "bounded legacy history"}`, async t => {
  let attached = 0
  let windows = 0
  const method = display ? "session.history.display" : "session.history"
  const chat = session(t, () => { throw new Error("unexpected history method") }, {
    "session.attach": params => {
      assert.deepEqual(params, { sessionId: "session", ...(display ? { display: true } : {}) })
      attached++
      return { providerId: "test", modelId: "test" }
    },
    [method]: params => {
      assert.equal(params.latestTurn, true)
      assert.equal(params.limit, 256)
      windows++
      return page([input(1, "stored"), event(2, { kind: "other" })], 2)
    },
  }, display)
  await flush()
  assert.equal(attached, 1)
  assert.equal(windows, 1)
  chat.receive({ method: display ? "session.display" : "session.event", sessionId: "session", branchId: "main", event: event(3, { kind: "other" }) })
  receive(chat, "main", input(4, "after compaction"))
  await flush()
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["stored", "after compaction"])
  assert.equal(chat.views.session.status, "ready")
})

test("a failed backward window remains retryable and never applies half a window", async t => {
  let calls = 0
  let fail = true
  const second = deferred()
  const chat = session(t, async ({ branchId, before, latestTurn }) => {
    if (branchId === "main") return page([], 0)
    calls++
    if (latestTurn) return { ...page([input(3, "newest stored")], 4), nextBefore: 3 }
    assert.equal(before, 3)
    if (fail) return second.promise
    return page([input(1, "older stored")], 4)
  })
  await flush()
  assert.equal(calls, 0, "directory discovery must not load a branch transcript")
  chat.ensureLine("session", "worker")
  await flush()
  receive(chat, "worker", input(5, "buffered"))
  second.reject(new RpcError(-32000, "read failed", "storage_failed"))
  await flush()
  assert.equal(chat.views.session.branches.worker.status, "failed")
  assert.deepEqual(chat.views.session.branches.worker.rows, [])
  receive(chat, "worker", input(6, "after failure"))
  fail = false
  chat.retryLine("session", "worker")
  await flush()
  assert.deepEqual(chat.views.session.branches.worker.rows.map(row => row.text), ["older stored", "newest stored", "buffered", "after failure"])
  assert.equal(chat.views.session.branches.worker.status, "ready")
})

test("reconnect keeps the newest unpersisted branch snapshot until history has loaded", async t => {
  let workerPage = page([], 0)
  const chat = session(t, ({ branchId }) => branchId === "main" ? page([], 0) : workerPage)
  await flush()
  chat.receive({ method: "session.state", sessionId: "session", branchId: "worker", running: true, stopping: false })
  await flush()
  chat.ensureLine("session", "worker")
  await flush()
  const held = deferred()
  workerPage = held.promise
  chat.resync()
  for (const text of ["old snapshot", "latest snapshot"]) chat.receive({ method: "session.model.progress", progress: { sessionId: "session", branchId: "worker", requestId: "r1", text, textOffset: 0, thinking: "", thinkingOffset: 0 } })
  await flush()
  held.resolve(page([], 0))
  await flush()
  assert.equal(chat.views.session.branches.worker.rows.find(row => row.kind === "assistant").text, "latest snapshot")

  let sequence = 0
  for (const [kind, terminalSource] of ["model-end", "tool-use", "turn-end"].flatMap(kind => ["notification", "history"].map(source => [kind, source]))) {
    const ending = deferred()
    workerPage = ending.promise
    chat.resync()
    chat.receive({ method: "session.model.progress", progress: { sessionId: "session", branchId: "worker", requestId: "r1", text: "unwritten tail", textOffset: 0, thinking: "", thinkingOffset: 0 } })
    await flush()
    const terminal = event(++sequence, kind === "turn-end" ? { kind, outcome: "completed" } : kind === "tool-use" ? { kind, requestId: "r1", callId: `call-${sequence}`, name: "read_file", input: {} } : { kind, requestId: "r1" })
    if (terminalSource === "notification") receive(chat, "worker", terminal)
    ending.resolve(page(terminalSource === "history" ? [terminal] : [], terminalSource === "history" ? terminal.sequence : terminal.sequence - 1))
    await flush()
    assert.equal(chat.views.session.branches.worker.rows.some(row => row.kind === "assistant" && row.streaming), false, "a terminal fact must not resurrect a buffered snapshot")
  }
})

test("main-history failure buffers live output until an explicit retry completes history", async t => {
  let fail = true
  const chat = session(t, () => {
    if (fail) throw new Error("session.history timed out")
    return page([input(1, "stored")], 1)
  })
  await flush()
  assert.equal(chat.views.session.status, "failed")
  receive(chat, "main", input(2, "new live event"))
  await flush()
  assert.deepEqual(chat.views.session.rows, [])
  fail = false
  chat.resync()
  await flush()
  assert.equal(chat.views.session.status, "ready")
  assert.equal(chat.views.session.rows[0].text, "stored")
})

test("a tool start drops a stale progress tail instead of reopening the thought", async t => {
  const chat = session(t, () => page([], 0))
  await flush()
  chat.receive({ method: "session.model.progress", progress: { sessionId: "session", branchId: "main", requestId: "r1", text: "", textOffset: 0, thinking: "weigh", thinkingOffset: 0, thinkingStartedAt: 1 } })
  receive(chat, "main", event(1, { kind: "thinking", requestId: "r1", delta: "weigh" }))
  receive(chat, "main", event(2, { kind: "tool-use-start", requestId: "r1", callId: "call", name: "echo" }))
  await flush()
  const thoughts = chat.views.session.rows.filter(row => row.kind === "thinking")
  assert.equal(thoughts.length, 1)
  assert.equal(thoughts[0].streaming, false)
})

test("a loaded tool start drops a stale progress tail instead of reopening the thought", async t => {
  const held = deferred()
  const chat = session(t, ({ branchId }) => branchId === "main" ? page([], 0) : held.promise)
  await flush()
  chat.receive({ method: "session.state", sessionId: "session", branchId: "worker", running: true, stopping: false })
  await flush()
  chat.ensureLine("session", "worker")
  await flush()
  chat.receive({ method: "session.model.progress", progress: { sessionId: "session", branchId: "worker", requestId: "r1", text: "", textOffset: 0, thinking: "weigh", thinkingOffset: 0, thinkingStartedAt: 1 } })
  await flush()
  held.resolve(page([
    event(1, { kind: "thinking", requestId: "r1", delta: "weigh" }),
    event(2, { kind: "tool-use-start", requestId: "r1", callId: "call", name: "echo" }),
  ], 2))
  await flush()
  const thoughts = chat.views.session.branches.worker.rows.filter(row => row.kind === "thinking")
  assert.equal(thoughts.length, 1)
  assert.equal(thoughts[0].streaming, false)
})

test("a request timeout leaves the socket usable and reports a failure rather than a disconnect", async t => {
  const NativeSocket = globalThis.WebSocket
  const nativeLocation = globalThis.location
  let socket
  globalThis.location = { protocol: "http:", host: "fixture" }
  globalThis.WebSocket = class {
    static OPEN = 1
    readyState = 1
    constructor() { socket = this }
    send() {}
    close() {}
  }
  t.after(() => { globalThis.WebSocket = NativeSocket; globalThis.location = nativeLocation })
  const client = new RpcClient({ phase() {}, ready() {}, notification() {}, invalid() {} })
  client.start()
  await assert.rejects(client.call("session.history", {}, x => x, 1), error => !(error instanceof ConnectionLost) && /timed out/.test(error.message))
  const next = client.call("daemon.ping", {}, x => x, 1000)
  socket.onmessage({ data: JSON.stringify({ jsonrpc: "2.0", id: 2, result: { ok: true } }) })
  assert.deepEqual(await next, { ok: true })
  client.stop()
})

test("panel activity includes stopping siblings and excludes ended branches", async t => {
  const chat = session(t, () => page([], 0))
  await flush()
  chat.receive({ method: "session.state", sessionId: "session", branchId: "worker", running: false, stopping: true })
  await flush()
  assert.equal(chat.views.session.running, false)
  assert.equal(chat.views.session.sessionRunning, true)
  receive(chat, "main", event(1, { kind: "branch-create", branchId: "worker", at: 0, prompt: "work", source: { kind: "host", operationId: "op" } }))
  receive(chat, "main", event(2, { kind: "branch-ended", branchId: "worker", source: { kind: "host", operationId: "end" } }))
  await flush()
  assert.equal(chat.views.session.sessionRunning, false)
})

test("initial reads stop at four raw pages, preserve invisible cursors and load earlier only on request", async t => {
  const calls = []
  const chat = session(t, params => {
    calls.push(params)
    const before = params.before ?? 10001
    return { ...page(before === 10001 ? [input(10000, "newest")] : [], 10000), nextBefore: before - 256 }
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  assert.equal(calls.length, 4)
  assert.equal(calls[0].latestTurn, true)
  assert.deepEqual(calls.slice(1).map(p => [p.before, p.through]), [[9745, 10000], [9489, 10000], [9233, 10000]])
  assert.equal(chat.views.session.earlier, true)
  await chat.loadEarlier("session", "main")
  assert.equal(calls.length, 8)
  assert.equal(calls[4].before, 8977)
  assert.equal(calls[4].latestTurn, undefined)
})

test("a window keeps an oversized wire page whole and restores controls from owner reads", async t => {
  let questions = 0
  const chat = session(t, () => ({ ...page(Array.from({ length: 350 }, (_, n) => input(2000 + n, `row ${n}`)), 3000), nextBefore: 2000 }), {
    "session.input.pending": p => ({ ...p, inputs: [{ id: "old-input", text: "queued before window", user: { content: [{ type: "text", text: "queued before window" }] } }], next: null }),
    "session.question.pending": p => {
      questions++
      return { ...p, asks: p.branchId === "worker" ? [{ requestId: "old-request", callId: "old-call", questions: [{ id: "q", question: "Still waiting?", options: [], multiSelect: false, allowCustom: true }] }] : [], next: null }
    },
    "session.goal.get": () => ({ objective: "old standing goal", status: { kind: "active" }, evaluator: false }),
  })
  await flush()
  assert.equal(chat.views.session.rows.length, 350)
  assert.equal(chat.views.session.queued[0].id, "old-input")
  assert.equal(chat.views.session.questions[0].branchId, "worker")
  assert.equal(chat.views.session.goal.objective, "old standing goal")
  assert.equal(questions, 2)
  assert.deepEqual(chat.views.session.branches.worker.rows, [])
  receive(chat, "worker", event(1, { kind: "tool-result", requestId: "old-request", results: [{ callId: "old-call", result: { content: [], isError: false } }] }))
  await flush()
  assert.equal(chat.views.session.questions.length, 0)
})

test("current output offsets do not duplicate an unwritten tail when the request began before the window", async t => {
  const chat = session(t, () => page([event(5000, { kind: "text", requestId: "request", delta: "suffix" })], 5000), {
    "session.turn.get": p => ({ ...p, turn: { sequence: 1, ended: false }, output: { requestId: "request", textCharacters: 1000, thinkingCharacters: 0 } }),
  })
  await flush()
  chat.receive({ method: "session.model.progress", progress: { sessionId: "session", branchId: "main", requestId: "request", text: "suffix tail", textOffset: 994, thinking: "", thinkingOffset: 0 } })
  await flush()
  assert.equal(chat.views.session.rows.find(row => row.kind === "assistant").text, "suffix tail")
})

test("historical and live results resolve only their missing call and keep event order", async t => {
  const origin = deferred()
  const calls = []
  const result = { content: [{ type: "text", text: "done" }] }
  const chat = session(t, () => page([event(100, { kind: "tool-result", requestId: "older", results: [{ callId: "old-call", result }] })], 100), {
    "session.tool.get": async params => {
      calls.push(params)
      if (params.callId === "live-call") return origin.promise
      return { requestId: "older", callId: "old-call", name: "read_file", input: { path: "older.rs" } }
    },
  })
  await flush()
  assert.equal(chat.views.session.rows[0].input.path, "older.rs")
  receive(chat, "main", event(101, { kind: "tool-completed", requestId: "live-request", callId: "live-call", result }))
  receive(chat, "main", input(102, "after result"))
  await flush()
  assert.equal(chat.views.session.rows.length, 1)
  origin.resolve({ requestId: "live-request", callId: "live-call", name: "exec", input: { command: "true" } })
  await flush()
  assert.deepEqual(chat.views.session.rows.map(row => row.kind), ["tool", "tool", "user"])
  assert.deepEqual(calls.map(p => [p.requestId, p.callId]), [["older", "old-call"], ["live-request", "live-call"]])
})

test("an unsupported server reports the missing window capability without reading history", async t => {
  let calls = 0
  const attached = deferred()
  const chat = session(t, () => { calls++; return page([], 0) }, { "session.attach": () => attached.promise })
  globalThis.historyStores.connection.supports = () => false
  attached.resolve({ providerId: "test", modelId: "test" })
  await flush()
  assert.equal(calls, 0)
  assert.equal(chat.views.session.status, "failed")
  assert.match(chat.views.session.error, /bounded conversation history/)
})

test("adoption leaves an unconsumed input pending until consumption or deletion", async t => {
  const chat = session(t, () => page([], 10), {
    "session.input.pending": p => ({ ...p, inputs: [{ id: "pending", text: "held", user: { content: [{ type: "text", text: "held" }] } }], next: null }),
  })
  await flush()
  receive(chat, "main", event(11, { kind: "input-applied", inputId: "pending" }))
  await flush()
  assert.equal(chat.views.session.queued[0].id, "pending")
  receive(chat, "main", event(12, { kind: "input-consumed", inputId: "pending" }))
  await flush()
  assert.deepEqual(chat.views.session.queued, [])
})

test("empty visible windows still pin the raw head before held live events", async t => {
  const pageWait = deferred()
  const chat = session(t, () => pageWait.promise)
  await flush()
  receive(chat, "main", input(50, "already included in snapshot"))
  receive(chat, "main", input(101, "after snapshot"))
  pageWait.resolve(page([], 100))
  await flush()
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["after snapshot"])
})


test("indexed transcript pages use one fixed head for initial and earlier reads", async t => {
  const requests = []
  const chat = session(t, params => {
    requests.push(params)
    assert.equal(params.tail, true)
    assert.equal(params.transcript, true)
    assert.equal(params.latestTurn, undefined)
    if (params.before === undefined) {
      assert.equal(params.limit, 32)
      return { ...page([], 100), nextBefore: 99 }
    }
    if (params.before === 99) {
      assert.equal(params.limit, 64)
      assert.equal(params.through, 100)
      return { ...page([input(90, "newest")], 100), nextBefore: 90 }
    }
    assert.equal(params.limit, 256)
    assert.equal(params.before, 90)
    assert.equal(params.through, 100)
    return { ...page([input(1, "oldest")], 100), nextBefore: 1 }
  }, {}, true, true)
  await flush()
  assert.equal(requests.length, 2)
  await chat.loadEarlier("session", "main")
  assert.equal(requests.length, 3)
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["oldest", "newest"])
})

test("a moved centre keeps the model its own timeline adopted", async t => {
  const chat = session(t, params => params.branchId === "worker"
    ? page([event(1, { kind: "context-configured", model: { providerId: "worker", modelId: "B" } })], 1)
    : page([], 0), {
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: "worker" }),
  })
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "worker")
  assert.deepEqual(
    chat.views.session.model,
    { providerId: "worker", modelId: "B" },
    "the centre keeps the model its own window adopted instead of the attach reply's, which is the root's (design §14)",
  )
})

test("a permission edit keeps the line whose policy it read", async t => {
  const held = deferred()
  const calls = []
  let arm = false
  const policy = (preset, tool) => ({ preset, allowedTools: [tool], disallowedTools: [] })
  const chat = session(t, params => params.branchId === "worker"
    ? page([input(1, "on the line")], 1)
    : page([input(1, "on the root")], 1), {
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: "main" }),
    "session.branch_set_default": () => "worker",
    "session.permission.get": async params => {
      calls.push(["get", params.branchId])
      if (arm) { arm = false; await held.promise }
      return { version: 0, policy: params.branchId === "worker" ? policy("read-only", "worker-tool") : policy("workspace-write", "main-tool") }
    },
    "session.permission.update": params => {
      calls.push(["update", params.branchId, params.expectedVersion, params.policy.allowedTools])
      return { version: 1 }
    },
  })
  const supports = globalThis.historyStores.connection.supports
  globalThis.historyStores.connection.supports = name => name === "session.permission.get" || name === "session.permission.update" || supports(name)
  await flush()
  arm = true
  const editing = chat.setPermission("session", "full-access")
  await flush()
  assert.deepEqual(calls.at(-1), ["get", "main"], "the edit reads the centre it started on")
  await chat.setDefaultBranch("session", "worker")
  assert.equal(chat.views.session.defaultBranchId, "worker")
  held.resolve()
  await editing
  assert.deepEqual(
    calls.filter(call => call[0] === "update"),
    [["update", "main", 0, ["main-tool"]]],
    "the write follows the branch whose policy was read, not the centre the move produced",
  )
  assert.deepEqual(chat.views.session.permission, policy("read-only", "worker-tool"), "the late reply does not become another centre's policy")
})

test("a move keeps the outgoing transcript and its target until the new centre is ready", async t => {
  const held = deferred()
  let hold = false
  const chat = session(t, async params => {
    if (params.branchId === "worker" && hold) await held.promise
    return params.branchId === "worker" ? page([input(7, "on the line")], 7) : page([input(1, "on the root")], 1)
  }, {
    "session.branch_set_default": () => "worker",
  })
  await flush()
  hold = true
  const moving = chat.setDefaultBranch("session", "worker")
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "main", "the centre still names the transcript on screen while the new one loads")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on the root"], "the old transcript stays until its replacement is ready")
  held.resolve()
  await moving
  assert.equal(chat.views.session.defaultBranchId, "worker")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on the line"], "the centre and its transcript change together")
})

test("a move to the root does not label the outgoing line's rows as root rows", async t => {
  const held = deferred()
  let hold = false
  const chat = session(t, params => {
    if (params.branchId === "main" && hold) return held.promise.then(() => page([input(1, "on the root")], 1))
    return params.branchId === "worker" ? page([input(7, "on the line")], 7) : page([input(1, "on the root")], 1)
  }, {
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 7, state: "open" }], defaultBranchId: "worker" }),
    "session.branch_set_default": () => "main",
  })
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "worker")
  hold = true
  const moving = chat.setDefaultBranch("session", "main")
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "worker", "the timeline still names the line whose rows it shows")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on the line"])
  assert.equal(chat.views.session.rows[0].cut, 6, "the shown rows keep their own line's cut")
  held.resolve()
  await moving
  assert.equal(chat.views.session.defaultBranchId, "main")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on the root"])
})

test("the root opens as its own line while another line is the centre", async t => {
  const chat = session(t, params => params.branchId === "worker"
    ? page([input(7, "on the line")], 7)
    : page([input(1, "on the root")], 1), {
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 7, state: "open" }], defaultBranchId: "worker" }),
    "session.question.pending": params => ({
      branchId: params.branchId,
      through: params.through,
      asks: params.branchId === "main" ? [{ requestId: "req-main", callId: "call-main", questions: [{ id: "q-1", question: "Which?", options: [{ label: "Yes" }, { label: "No" }], multiSelect: false, allowCustom: false }] }] : [],
      next: null,
    }),
  })
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "worker")
  chat.ensureLine("session", "main")
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "worker", "reading the root leaves the Session's default where it was")
  assert.equal(chat.views.session.branches.main?.status, "ready", "the root's own window loads like any opened line's")
  assert.deepEqual(chat.views.session.branches.main?.rows.map(row => row.text), ["on the root"])
  assert.deepEqual(chat.views.session.questions.map(ask => [ask.branchId, ask.requestId]), [["main", "req-main"]], "the root's pending question is restored for its own line")
})

test("a resync that discovers a moved default shows its pending question once", async t => {
  let defaultId = "main"
  const chat = session(t, params => params.branchId === "worker"
    ? page([input(1, "on the line")], 1)
    : page([input(1, "on the root")], 1), {
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: defaultId }),
    "session.question.pending": params => ({
      branchId: params.branchId,
      through: params.through,
      asks: params.branchId === "worker" ? [{ requestId: "req-1", callId: "call-1", questions: [{ id: "q-1", question: "Which?", options: [{ label: "Yes" }, { label: "No" }], multiSelect: false, allowCustom: false }] }] : [],
      next: null,
    }),
  })
  await flush()
  chat.ensureLine("session", "worker")
  await flush()
  assert.equal(chat.views.session.questions.filter(ask => ask.branchId === "worker").length, 1, "the opened line holds its question once")
  defaultId = "worker"
  await chat.resync()
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "worker")
  assert.equal(chat.views.session.questions.filter(ask => ask.branchId === "worker").length, 1, "the newly centred line's question appears once")
})
