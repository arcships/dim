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
const goalInput = (sequence, text) => event(sequence, { kind: "input", text, content: [{ type: "text", text }], blocks: [{ type: "raw", text, key: "goal:1", retire: false }] })
const page = (events, through, nextAfter = through, done = true) => ({ events, through, nextAfter, done })
const flush = () => new Promise(resolve => setTimeout(resolve, 25))
/** Waits for a publish the store schedules on its own timer, rather than guessing how long it takes. */
const until = async (condition, timeout = 2000) => {
  const deadline = Date.now() + timeout
  while (!condition()) {
    if (Date.now() > deadline) return
    await new Promise(resolve => setTimeout(resolve, 5))
  }
}
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
      if (method === "session.model.get") return { branchId: params.branchId, through: params.through ?? 0 }
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

test("a failed move recovers on the accepted line when the failed page is retried", async t => {
  let fail = true
  let moves = 0
  let reads = 0
  const chat = session(t, params => {
    if (params.branchId !== "worker") return page([input(1, "on the root")], 1)
    reads++
    if (fail) throw new Error("the line could not be read")
    return page([input(1, "on the worker")], 1)
  }, {
    "session.branch_set_default": () => {
      moves++
      return "worker"
    },
  })
  await flush()
  await chat.setDefaultBranch("session", "worker")
  assert.equal(chat.views.session.status, "failed")
  receive(chat, "worker", input(2, "live while failed"))
  await flush()
  assert.deepEqual(chat.views.session.rows, [], "the failed centre shows no transcript")
  fail = false
  chat.retryCentre("session")
  chat.retryCentre("session")
  await flush()
  assert.equal(moves, 1, "the retry reads the accepted line instead of moving again")
  assert.equal(reads, 2, "a second click during the retry starts no second read")
  assert.equal(chat.views.session.status, "ready")
  assert.equal(chat.views.session.defaultBranchId, "worker")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on the worker", "live while failed"], "the events buffered by the failure are replayed after the retry's window")
})

test("a Session whose first sync failed retries the whole attach", async t => {
  let attaches = 0
  const chat = session(t, () => page([input(1, "stored")], 1), {
    "session.attach": () => {
      attaches++
      if (attaches === 1) throw new Error("attach failed")
      return { providerId: "test", modelId: "test" }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "failed")
  chat.retryCentre("session")
  await flush()
  assert.equal(attaches, 2, "before the directory is read, the retry repeats the attach")
  assert.equal(chat.views.session.status, "ready")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["stored"])
})

test("a retry a later move overtakes does not replace the line that moved", async t => {
  const held = deferred()
  let fail = true
  let retrying = false
  const chat = session(t, params => {
    if (params.branchId !== "worker") return page([input(1, `on ${params.branchId}`)], 1)
    if (fail) throw new Error("the line could not be read")
    if (retrying) return held.promise
    return page([input(1, "on the worker")], 1)
  }, {
    "session.branch_set_default": params => params.branchId,
  })
  await flush()
  await chat.setDefaultBranch("session", "worker")
  assert.equal(chat.views.session.status, "failed")
  fail = false
  retrying = true
  chat.retryCentre("session")
  await flush()
  await chat.setDefaultBranch("session", "third")
  retrying = false
  held.resolve(page([input(1, "on the worker")], 1))
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "third")
  assert.equal(chat.views.session.status, "ready")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on third"], "the overtaken retry's window is discarded")
})

test("a reconnect that overlaps a retry reads after it, not beside it", async t => {
  const held = deferred()
  let fail = true
  let retrying = false
  let attaches = 0
  let directory = "main"
  const chat = session(t, params => {
    if (params.branchId !== "worker") return page([], 0)
    if (fail) throw new Error("the line could not be read")
    if (retrying) return held.promise
    return page([input(1, "on the worker")], 1)
  }, {
    "session.branch_set_default": () => "worker",
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: directory }),
    "session.attach": () => {
      attaches++
      return { providerId: "test", modelId: "test" }
    },
  })
  await flush()
  await chat.setDefaultBranch("session", "worker")
  assert.equal(chat.views.session.status, "failed")
  fail = false
  retrying = true
  directory = "worker"
  chat.retryCentre("session")
  await flush()
  void chat.resync()
  await flush()
  assert.equal(attaches, 1, "the reconnect waits for the retry instead of attaching beside it")
  retrying = false
  held.resolve(page([input(1, "on the worker")], 1))
  await flush()
  assert.equal(attaches, 2, "the reconnect runs once the retry settles")
  assert.equal(chat.views.session.status, "ready")
  assert.equal(chat.views.session.defaultBranchId, "worker")
})

test("a failed model read during sync keeps the centre readable", async t => {
  const chat = session(t, params => params.branchId === "worker"
    ? page([input(1, "on the worker")], 1)
    : page([], 0), {
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: "worker" }),
    "session.model.get": () => {
      throw new Error("the model read failed")
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready", "a failed model read alone does not fail the page")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on the worker"])
  assert.equal(chat.views.session.model, undefined)
})

test("a move whose goal refresh fails keeps its failure and reads no policy", async t => {
  const policy = { preset: "workspace-write", allowedTools: [], disallowedTools: [] }
  let failGoal = false
  let permissions = 0
  const chat = session(t, params => page([input(1, `on ${params.branchId}`)], 1), {
    "session.branch_set_default": () => {
      failGoal = true
      return "worker"
    },
    "session.goal.get": params => {
      if (failGoal) throw new Error("the goal read failed")
      return { branchId: params.branchId, through: 1, goal: null }
    },
    "session.permission.get": () => {
      permissions++
      return { version: 1, policy }
    },
  })
  const supports = globalThis.historyStores.connection.supports
  globalThis.historyStores.connection.supports = name => name === "session.permission.get" || supports(name)
  await flush()
  assert.equal(permissions, 1, "the first sync reads the policy once")
  await chat.setDefaultBranch("session", "worker")
  assert.equal(chat.views.session.status, "failed")
  assert.equal(chat.views.session.error, "the goal read failed")
  assert.equal(permissions, 1, "a failed goal refresh ends the move before its policy read")
  assert.equal(chat.views.session.permission, undefined)
  failGoal = false
  chat.retryCentre("session")
  await flush()
  assert.equal(chat.views.session.status, "ready")
  assert.deepEqual(chat.views.session.permission, policy)
})

test("a Session that closes during a retry stays closed", async t => {
  const held = deferred()
  let fail = true
  let retrying = false
  const chat = session(t, () => {
    if (fail) throw new Error("the line could not be read")
    return page([input(1, "on the line")], 1)
  }, {
    "session.goal.get": params => retrying ? held.promise : { branchId: params.branchId, through: 1, goal: null },
  })
  await flush()
  assert.equal(chat.views.session.status, "failed")
  fail = false
  retrying = true
  chat.retryCentre("session")
  await flush()
  chat.receive({ method: "session.closed", sessionId: "session" })
  assert.equal(chat.views.session.status, "closed")
  held.resolve({ branchId: "main", through: 1, goal: null })
  await flush()
  assert.equal(chat.views.session.status, "closed", "a Session closed during the retry does not become ready again")
})

test("a Session that closes while a reconnect waits for a retry stays closed", async t => {
  const held = deferred()
  let fail = true
  let hold = false
  let attaches = 0
  const chat = session(t, () => {
    if (hold) return held.promise
    if (fail) throw new Error("the line could not be read")
    return page([input(1, "on the line")], 1)
  }, {
    "session.attach": () => {
      attaches++
      return { providerId: "test", modelId: "test" }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "failed")
  fail = false
  hold = true
  chat.retryCentre("session")
  await flush()
  chat.receive({ method: "session.closed", sessionId: "session" })
  void chat.resync()
  await flush()
  assert.equal(attaches, 1, "the reconnect waits for the retry")
  hold = false
  held.resolve(page([input(1, "on the line")], 1))
  await flush()
  assert.equal(chat.views.session.status, "closed", "a reconnect does not open a closed Session again")
  assert.equal(attaches, 1, "a closed Session is not attached again")
})

test("a retry after a reconnect that could not attach attaches again", async t => {
  let attaches = 0
  let attachable = true
  const chat = session(t, () => page([input(1, "stored")], 1), {
    "session.attach": () => {
      attaches++
      if (!attachable) throw new Error("the attach failed")
      return { providerId: "test", modelId: "test" }
    },
  })
  await flush()
  assert.equal(attaches, 1)
  assert.equal(chat.views.session.status, "ready")
  attachable = false
  await chat.resync()
  assert.equal(attaches, 2, "the reconnect attaches again")
  assert.equal(chat.views.session.status, "failed")
  attachable = true
  chat.retryCentre("session")
  await flush()
  assert.equal(attaches, 3, "the retry repeats the attach instead of reading beside a connection without one")
  assert.equal(chat.views.session.status, "ready")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["stored"])
})

test("a retry after a reconnect that cannot read the directory attaches again", async t => {
  let attaches = 0
  let directory = true
  const chat = session(t, () => page([input(1, "stored")], 1), {
    "session.attach": () => {
      attaches++
      return { providerId: "test", modelId: "test" }
    },
    "session.branches": () => {
      if (!directory) throw new Error("the directory could not be read")
      return { branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: "main" }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  directory = false
  await chat.resync()
  assert.equal(attaches, 2, "the reconnect attaches again")
  assert.equal(chat.views.session.status, "failed")
  directory = true
  chat.retryCentre("session")
  await flush()
  assert.equal(attaches, 3, "a directory read that did not finish sends the retry back through the attach")
  assert.equal(chat.views.session.status, "ready")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["stored"])
})

test("a retry after a reconnect whose pending questions failed attaches again", async t => {
  let attaches = 0
  let questions = true
  const chat = session(t, () => page([input(1, "stored")], 1), {
    "session.attach": () => {
      attaches++
      return { providerId: "test", modelId: "test" }
    },
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: "main" }),
    "session.question.pending": params => {
      if (!questions) throw new Error("the questions could not be read")
      return { ...params, asks: [], next: null }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  questions = false
  await chat.resync()
  assert.equal(attaches, 2, "the reconnect attaches again")
  assert.equal(chat.views.session.status, "failed")
  questions = true
  chat.retryCentre("session")
  await flush()
  assert.equal(attaches, 3, "a pending-question read that did not finish sends the retry back through the attach")
  assert.equal(chat.views.session.status, "ready")
})

test("a reconnect that waits for a retry keeps the events buffered during it", async t => {
  const held = deferred()
  let fail = true
  let hold = false
  let attaches = 0
  const chat = session(t, () => {
    if (hold) return held.promise
    if (fail) throw new Error("the line could not be read")
    return page([], 0)
  }, {
    "session.attach": () => {
      attaches++
      return { providerId: "test", modelId: "test" }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "failed")
  fail = false
  hold = true
  chat.retryCentre("session")
  await flush()
  receive(chat, "main", input(1, "buffered while the retry ran"))
  await flush()
  void chat.resync()
  void chat.resync()
  await flush()
  assert.equal(attaches, 1, "the reconnects wait for the retry")
  hold = false
  held.reject(new Error("the line could not be read"))
  await flush()
  assert.equal(attaches, 2, "one reconnect attaches once the retry settles")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["buffered while the retry ran"], "the events buffered while the reconnect waited are not dropped")
})

test("a goal refresh reports neither a closed Session nor a lost connection as a failure", async t => {
  let goal = "ok"
  const chat = session(t, () => page([input(1, "stored")], 1), {
    "session.goal.get": params => {
      if (goal === "lost") throw new ConnectionLost()
      if (goal === "failed") throw new Error("the goal read failed")
      return { branchId: params.branchId, through: 1, goal: null }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  goal = "lost"
  receive(chat, "main", goalInput(2, "keep going"))
  await flush()
  assert.equal(chat.views.session.status, "ready", "a lost connection is not a failed page")
  goal = "failed"
  chat.receive({ method: "session.closed", sessionId: "session" })
  assert.equal(chat.views.session.status, "closed")
  receive(chat, "main", goalInput(3, "keep going again"))
  await flush()
  assert.equal(chat.views.session.status, "closed", "a read after the Session closed keeps it closed")
})

test("a goal read that reports the Session closed closes the page", async t => {
  let goal = "ok"
  const chat = session(t, () => page([input(1, "stored")], 1), {
    "session.goal.get": params => {
      if (goal === "closed") throw new RpcError(-32000, "the Session is closed", "session_closed")
      return { branchId: params.branchId, through: 1, goal: null }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  goal = "closed"
  receive(chat, "main", goalInput(2, "keep going"))
  await flush()
  assert.equal(chat.views.session.status, "closed", "a goal read that reports the Session closed closes the page")
})

test("a retry loads the side lines an earlier sync left waiting", async t => {
  let fail = false
  const chat = session(t, params => {
    if (params.branchId === "worker") return page([input(1, "on the worker")], 1)
    if (fail) throw new Error("the centre could not be read")
    return page([input(1, "on the root")], 1)
  }, {
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: "main" }),
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  chat.ensureLine("session", "worker")
  await flush()
  assert.deepEqual(chat.views.session.branches.worker.rows.map(row => row.text), ["on the worker"])
  fail = true
  await chat.resync()
  assert.equal(chat.views.session.status, "failed")
  assert.equal(chat.views.session.branches.worker.status, "loading", "the side line is still waiting for its window")
  fail = false
  chat.retryCentre("session")
  await flush()
  assert.equal(chat.views.session.status, "ready")
  assert.equal(chat.views.session.branches.worker.status, "ready", "the retry loads the side line the earlier sync queued")
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
  await until(() => chat.views.session.rows.length === 2)
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
  // The production display transcript omits `context/configured`, so a bounded
  // window cannot carry a line's adoption: the centre reads that line's own
  // configuration through `session.model.get` at the window's cutoff.
  const reads = []
  const chat = session(t, params => page([], params.branchId === "worker" ? 7 : 0), {
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 7, state: "open" }], defaultBranchId: "worker" }),
    "session.model.get": params => {
      reads.push(params)
      return { branchId: "worker", through: 7, configuration: { model: { providerId: "worker", modelId: "B" } } }
    },
    "session.history": () => { throw new Error("a line's configuration is not paged out of its history") },
  })
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "worker")
  assert.deepEqual(reads, [{ sessionId: "session", branchId: "worker", through: 7 }], "one branch-qualified read at the window's cutoff, with no page cursor")
  assert.deepEqual(
    chat.views.session.model,
    { providerId: "worker", modelId: "B" },
    "the centre keeps the model its own line adopted instead of the attach reply's, which is the root's (design §14)",
  )
})

test("a centre on a line shows no model when the server cannot read that line's configuration", async t => {
  const chat = session(t, params => page([], params.branchId === "worker" ? 7 : 0), {
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 7, state: "open" }], defaultBranchId: "worker" }),
  })
  globalThis.historyStores.sessions.byId = new Map([["session", { model: { providerId: "root", modelId: "A" } }]])
  const supports = globalThis.historyStores.connection.supports
  globalThis.historyStores.connection.supports = name => name !== "session.model.get" && supports(name)
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "worker")
  assert.equal(
    chat.views.session.model,
    undefined,
    "the directory's model is the root's, so it does not stand in for the centred line's (design §14)",
  )
})

test("a refused move leaves an earlier accepted move to finish", async t => {
  const workerPage = deferred()
  const chat = session(t, params => params.branchId === "worker"
    ? workerPage.promise
    : page([input(1, "on the root")], 1), {
    "session.branches": () => ({
      branches: [
        { branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 7, state: "open" },
        { branchId: "other", parentBranchId: "main", branchPoint: 0, head: 3, state: "open" },
      ],
      defaultBranchId: "main",
    }),
    "session.branch_set_default": params => {
      if (params.branchId === "worker") return "worker"
      throw new RpcError(-32000, "fixture: the move was refused", "branch_not_opener")
    },
    "session.model.get": params => ({ branchId: params.branchId, through: params.through }),
    "session.goal.get": params => ({ branchId: params.branchId, through: 0, goal: null }),
  })
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "main")
  const moved = chat.setDefaultBranch("session", "worker")
  await flush()
  // The refused move is the later one, and the earlier move's window is still
  // loading: the server's default is already `worker`, so its completion must
  // not be discarded as stale (design §14).
  await assert.rejects(chat.setDefaultBranch("session", "other"), /the move was refused/)
  workerPage.resolve(page([input(1, "on the line")], 7))
  await moved
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "worker")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on the line"], "the centre is the move the server accepted")
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

test("a late history load cannot undo a newer default move", async t => {
  const held = deferred()
  let hold = false
  const chat = session(t, async params => {
    if (params.branchId === "worker" && hold) await held.promise
    const sequence = params.branchId === "worker" ? 7 : 9
    return page([input(sequence, params.branchId)], sequence)
  }, {
    "session.branches": () => ({ branches: [
      { branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 7, state: "open" },
      { branchId: "other", parentBranchId: "main", branchPoint: 0, head: 9, state: "open" },
    ], defaultBranchId: "main" }),
    "session.branch_set_default": params => params.branchId,
  })
  await flush()
  hold = true
  const first = chat.setDefaultBranch("session", "worker")
  await flush()
  await chat.setDefaultBranch("session", "other")
  assert.equal(chat.views.session.defaultBranchId, "other", "the newer move is the centre")
  held.resolve()
  await first
  assert.equal(chat.views.session.defaultBranchId, "other", "the earlier load finishing late does not revert the move")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["other"], "the transcript stays the newer centre's")
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

test("a reconnect whose goal read fails keeps an approval it queued", async t => {
  const attach = deferred()
  let attaches = 0
  let failGoal = false
  const approval = { sessionId: "session", branchId: "main", requestId: "req-1", call: { name: "shell", input: {} }, reason: "fixture" }
  const chat = session(t, () => page([input(1, "stored")], 1), {
    "session.attach": () => {
      attaches++
      return attaches === 1 ? { providerId: "test", modelId: "test" } : attach.promise
    },
    "session.goal.get": params => {
      if (failGoal) throw new Error("the goal read failed")
      return { branchId: params.branchId, through: 1, goal: null }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  failGoal = true
  const reconnecting = chat.resync()
  await flush()
  chat.receive({ method: "approval.request", request: approval })
  attach.resolve({ providerId: "test", modelId: "test" })
  await reconnecting
  await flush()
  assert.equal(chat.views.session.status, "failed")
  failGoal = false
  chat.retryCentre("session")
  await flush()
  assert.equal(chat.views.session.status, "ready")
  assert.deepEqual(chat.views.session.approvals.map(request => request.requestId), ["req-1"], "the retry shows the approval the failed reconnect had queued")
})

test("a reconnect whose goal read fails keeps a queued close", async t => {
  const attach = deferred()
  let attaches = 0
  let failGoal = false
  const chat = session(t, () => page([input(1, "stored")], 1), {
    "session.attach": () => {
      attaches++
      return attaches === 1 ? { providerId: "test", modelId: "test" } : attach.promise
    },
    "session.goal.get": params => {
      if (failGoal) throw new Error("the goal read failed")
      return { branchId: params.branchId, through: 1, goal: null }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  failGoal = true
  const reconnecting = chat.resync()
  await flush()
  chat.receive({ method: "session.closed", sessionId: "session" })
  attach.resolve({ providerId: "test", modelId: "test" })
  await reconnecting
  await flush()
  failGoal = false
  chat.retryCentre("session")
  await flush()
  assert.equal(chat.views.session.status, "closed", "a close queued behind a failed goal read is not dropped")
})

test("a reconnect whose goal read fails keeps the events it buffered", async t => {
  const attach = deferred()
  let attaches = 0
  let failGoal = false
  const chat = session(t, () => page([input(1, "stored")], 1), {
    "session.attach": () => {
      attaches++
      return attaches === 1 ? { providerId: "test", modelId: "test" } : attach.promise
    },
    "session.goal.get": params => {
      if (failGoal) throw new Error("the goal read failed")
      return { branchId: params.branchId, through: 1, goal: null }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  failGoal = true
  const reconnecting = chat.resync()
  await flush()
  receive(chat, "main", input(2, "arrived while reconnecting"))
  attach.resolve({ providerId: "test", modelId: "test" })
  await reconnecting
  await flush()
  assert.equal(chat.views.session.status, "failed")
  failGoal = false
  chat.retryCentre("session")
  await flush()
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["stored", "arrived while reconnecting"], "the events the failed reconnect had queued are replayed by the retry")
})

test("a reconnect whose goal read loses the connection is not ready", async t => {
  let lost = false
  const chat = session(t, () => page([input(1, "stored")], 1), {
    "session.goal.get": params => {
      if (lost) throw new ConnectionLost()
      return { branchId: params.branchId, through: 1, goal: null }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  lost = true
  await chat.resync()
  await flush()
  assert.equal(chat.views.session.status, "loading", "a goal read that lost the connection leaves the page to the reconnect")
  lost = false
  await chat.resync()
  await flush()
  assert.equal(chat.views.session.status, "ready")
})

test("a move waits for a goal refresh already running on its line", async t => {
  const held = deferred()
  let arm = false
  const policies = []
  const policy = { preset: "read-only", allowedTools: [], disallowedTools: [] }
  const chat = session(t, params => page([input(1, `on ${params.branchId}`)], 1), {
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: "main" }),
    "session.branch_set_default": () => "worker",
    "session.goal.set": () => ({}),
    "session.goal.get": params => {
      if (arm && params.branchId === "worker") return held.promise
      return { branchId: params.branchId, through: 1, goal: null }
    },
    "session.permission.get": params => {
      policies.push(params.branchId)
      return { version: 1, policy }
    },
  })
  const supports = globalThis.historyStores.connection.supports
  globalThis.historyStores.connection.supports = name => name === "session.permission.get" || supports(name)
  await flush()
  assert.deepEqual(policies, ["main"], "the first sync reads the centre's policy")
  arm = true
  const writing = chat.writeGoal("session", "set", "carry on", "worker").catch(() => undefined)
  await flush()
  const moving = chat.setDefaultBranch("session", "worker")
  await flush()
  assert.deepEqual(policies, ["main"], "the move waits for the goal refresh already running on its line")
  held.reject(new Error("the goal read failed"))
  await moving
  await writing
  await flush()
  assert.equal(chat.views.session.status, "failed")
  assert.deepEqual(policies, ["main"], "a failed goal refresh ends the move before its policy read")
})

test("a move whose goal read loses the connection is not ready and reads no policy", async t => {
  let lost = false
  const policies = []
  const policy = { preset: "read-only", allowedTools: [], disallowedTools: [] }
  const chat = session(t, params => page([input(1, `on ${params.branchId}`)], 1), {
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: "main" }),
    "session.branch_set_default": () => "worker",
    "session.goal.get": params => {
      if (lost) throw new ConnectionLost()
      return { branchId: params.branchId, through: 1, goal: null }
    },
    "session.permission.get": params => {
      policies.push(params.branchId)
      return { version: 1, policy }
    },
  })
  const supports = globalThis.historyStores.connection.supports
  globalThis.historyStores.connection.supports = name => name === "session.permission.get" || supports(name)
  await flush()
  assert.deepEqual(policies, ["main"])
  lost = true
  await chat.setDefaultBranch("session", "worker")
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "worker", "the accepted move stands")
  assert.equal(chat.views.session.status, "loading", "a goal read that lost the connection leaves the page to the reconnect, not ready")
  assert.deepEqual(policies, ["main"], "a goal read that lost the connection ends the move before its policy read")
})

test("a move whose history read loses the connection is not ready", async t => {
  let lost = false
  let moved = false
  const chat = session(t, params => {
    if (lost && params.branchId === "worker") throw new ConnectionLost()
    return page([input(1, `on ${params.branchId}`)], 1)
  }, {
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: moved ? "worker" : "main" }),
    "session.branch_set_default": () => {
      moved = true
      return "worker"
    },
    "session.goal.get": params => ({ branchId: params.branchId, through: 1, goal: null }),
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  lost = true
  await chat.setDefaultBranch("session", "worker")
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "worker", "the accepted move stands")
  assert.equal(chat.views.session.status, "loading", "a lost history read leaves the centre to the reconnect instead of a ready page with no messages")
  assert.deepEqual(chat.views.session.rows, [], "the centre has no transcript to show")
  lost = false
  await chat.resync()
  await flush()
  assert.equal(chat.views.session.status, "ready", "the reconnect's own sync reads the centre that lost the connection")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on worker"])
})

test("a permission read that lands after a failed goal refresh does not make the page ready", async t => {
  const held = deferred()
  const policy = { preset: "read-only", allowedTools: [], disallowedTools: [] }
  const newer = { preset: "full-access", allowedTools: [], disallowedTools: [] }
  let arm = false
  let failGoal = false
  const chat = session(t, () => page([input(1, "stored")], 1), {
    "session.goal.get": params => {
      if (failGoal) throw new Error("the goal read failed")
      return { branchId: params.branchId, through: 1, goal: null }
    },
    "session.permission.get": () => {
      if (!arm) return { version: 1, policy }
      // The read is out after the sync settled its history and goal, and the
      // centre's goal event arrives while it is.
      failGoal = true
      return held.promise
    },
  })
  const supports = globalThis.historyStores.connection.supports
  globalThis.historyStores.connection.supports = name => name === "session.permission.get" || supports(name)
  await flush()
  assert.equal(chat.views.session.status, "ready")
  arm = true
  const reconnecting = chat.resync()
  await flush()
  receive(chat, "main", goalInput(2, "goal: carry on"))
  await flush()
  assert.equal(chat.views.session.status, "failed", "the centre's goal read failed while the permission read was out")
  held.resolve({ version: 2, policy: newer })
  await reconnecting
  await flush()
  assert.equal(chat.views.session.status, "failed", "a permission read that lands later does not publish a ready page over the failure")
  assert.equal(chat.views.session.error, "the goal read failed")
  assert.deepEqual(chat.views.session.permission, policy, "the failed page keeps the policy it showed before the failure, not the read that crossed it")
  assert.equal(chat.views.session.permissionVersion, 1)
  failGoal = false
  arm = false
  chat.retryCentre("session")
  await flush()
  assert.equal(chat.views.session.status, "ready", "the Retry the failed page offered recovers it")
  assert.deepEqual(chat.views.session.permission, policy)
})

test("a concurrent goal refresh reads the line each caller asked for", async t => {
  const held = deferred()
  let arm = false
  const reads = []
  const chat = session(t, params => page([input(1, `on ${params.branchId}`)], 1), {
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: "main" }),
    "session.goal.set": () => ({}),
    // The store keeps the goal a read decodes, so the fixture answers with one.
    "session.goal.get": params => {
      reads.push(params.branchId)
      if (arm && params.branchId === "worker") return held.promise
      return params.branchId === "main" ? { objective: "root work", evaluator: false, status: { kind: "active" } } : undefined
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  assert.deepEqual(reads, ["main"])
  arm = true
  const writing = chat.writeGoal("session", "set", "worker work", "worker")
  await flush()
  const reconnecting = chat.resync()
  await flush()
  const rewriting = chat.writeGoal("session", "set", "worker again", "worker")
  await flush()
  held.resolve(undefined)
  await reconnecting
  await writing
  await rewriting
  await flush()
  assert.equal(chat.views.session.status, "ready")
  assert.equal(reads.filter(line => line === "main").length, 2, "the reconnect reads the centre it asked for, not only the last caller's line")
  assert.equal(chat.views.session.goal?.objective, "root work", "the reconnect shows the centre's own goal")
})

test("a Session that closes while the retry reads the policy stays closed", async t => {
  const held = deferred()
  let fail = true
  let arm = false
  const policy = { preset: "read-only", allowedTools: [], disallowedTools: [] }
  const chat = session(t, () => {
    if (fail) throw new Error("the line could not be read")
    return page([input(1, "on the line")], 1)
  }, {
    "session.goal.get": params => ({ branchId: params.branchId, through: 1, goal: null }),
    "session.permission.get": () => arm ? held.promise : { version: 1, policy },
  })
  const supports = globalThis.historyStores.connection.supports
  globalThis.historyStores.connection.supports = name => name === "session.permission.get" || supports(name)
  await flush()
  assert.equal(chat.views.session.status, "failed")
  fail = false
  arm = true
  chat.retryCentre("session")
  await flush()
  chat.receive({ method: "session.closed", sessionId: "session" })
  assert.equal(chat.views.session.status, "closed")
  held.resolve({ version: 1, policy })
  await flush()
  assert.equal(chat.views.session.status, "closed", "a Session closed during the policy read stays closed")
})

test("a resync whose history read fails after another line took the centre leaves the moved page to its own reads", async t => {
  const rootPage = deferred()
  const workerPage = deferred()
  const workerGoal = deferred()
  let defaultId = "main"
  let rootReads = 0
  let workerFails = false
  const chat = session(t, params => {
    if (params.branchId === "worker") {
      if (workerFails) throw new Error("the centred line could not be read")
      return workerPage.promise
    }
    rootReads++
    return rootReads === 1 ? page([input(1, "on the root")], 1) : rootPage.promise
  }, {
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: defaultId }),
    "session.branch_set_default": () => {
      defaultId = "worker"
      return "worker"
    },
    "session.goal.get": params => params.branchId === "worker" ? workerGoal.promise : { branchId: "main", through: 1, goal: null },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  const reconnecting = chat.resync()
  await until(() => rootReads === 2)
  const moving = chat.setDefaultBranch("session", "worker")
  await flush()
  workerPage.resolve(page([input(1, "on the worker")], 1))
  await until(() => chat.views.session.defaultBranchId === "worker")
  await flush()
  rootPage.reject(new Error("the line that moved away could not be read"))
  await reconnecting
  await flush()
  assert.equal(chat.views.session.status, "loading", "the outgoing line's late failure does not fail the moved page")
  assert.equal(chat.views.session.error, "", "the outgoing line's failure is not reported for the centre")
  workerGoal.resolve({ branchId: "worker", through: 1, goal: null })
  await moving
  await flush()
  assert.equal(chat.views.session.status, "ready", "the moved page leaves loading through the reads of the centre it shows")
  assert.equal(chat.views.session.error, "")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on the worker"])
  workerFails = true
  await chat.resync()
  await flush()
  assert.equal(chat.views.session.status, "failed", "a read of the centre itself still fails the page")
  assert.equal(chat.views.session.error, "the centred line could not be read")
})

test("a resync whose directory answer predates a move does not take the centre back", async t => {
  const directory = deferred()
  let directories = 0
  const chat = session(t, params => page([input(1, `on ${params.branchId}`)], 1), {
    // The resync's answer was read before the move was accepted and still names the old default.
    "session.branches": () => {
      directories++
      return directories === 1
        ? { branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: "main" }
        : directory.promise
    },
    "session.branch_set_default": () => "worker",
    "session.goal.get": params => ({ branchId: params.branchId, through: 1, goal: null }),
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  const reconnecting = chat.resync()
  await until(() => directories === 2)
  await chat.setDefaultBranch("session", "worker")
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "worker", "the move is the centre")
  directory.resolve({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: "main" })
  await reconnecting
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "worker", "a directory answer read before the move does not take the centre back")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on worker"], "the page keeps the transcript the move read")
})

test("a resync's ready page does not land on a page a move took while its policy read was out", async t => {
  const policy = deferred()
  const workerGoal = deferred()
  const read = { version: 1, policy: { preset: "read-only", allowedTools: [], disallowedTools: [] } }
  let armed = false
  let policyReads = 0
  const chat = session(t, params => page([input(1, `on ${params.branchId}`)], 1), {
    "session.branches": () => ({ branches: [{ branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 1, state: "open" }], defaultBranchId: "main" }),
    "session.branch_set_default": () => "worker",
    "session.permission.get": () => {
      if (!armed) return read
      policyReads++
      return policy.promise
    },
    "session.goal.get": params => params.branchId === "worker" ? workerGoal.promise : { branchId: "main", through: 1, goal: null },
  })
  const supports = globalThis.historyStores.connection.supports
  globalThis.historyStores.connection.supports = name => name === "session.permission.get" || supports(name)
  await flush()
  assert.equal(chat.views.session.status, "ready")
  armed = true
  const reconnecting = chat.resync()
  await until(() => policyReads === 1)
  const moving = chat.setDefaultBranch("session", "worker")
  await until(() => chat.views.session.defaultBranchId === "worker")
  policy.resolve({ version: 2, policy: read.policy })
  await flush()
  assert.equal(chat.views.session.status, "loading", "the ready page a resync would publish does not land on the page the move took")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on worker"], "the moved page keeps the transcript the move read")
  workerGoal.resolve({ branchId: "worker", through: 1, goal: null })
  await moving
  await reconnecting
  await flush()
  assert.equal(chat.views.session.status, "ready", "the moved page leaves loading through its own reads")
  assert.deepEqual(chat.views.session.permission, read.policy)
  assert.equal(chat.views.session.permissionVersion, 2)
})

test("a side line keeps the events it received while the centre page was failed", async t => {
  let failGoal = false
  const chat = session(t, params => page([input(1, `on ${params.branchId}`)], 1), {
    "session.goal.get": params => {
      if (failGoal && params.branchId === "main") throw new Error("the goal read failed")
      return { branchId: params.branchId, through: 1, goal: null }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  chat.ensureLine("session", "worker")
  await flush()
  assert.deepEqual(chat.views.session.branches.worker.rows.map(row => row.text), ["on worker"])
  failGoal = true
  receive(chat, "main", goalInput(2, "goal: carry on"))
  await flush()
  assert.equal(chat.views.session.status, "failed", "a goal event whose read fails reports the page failed")
  receive(chat, "worker", input(2, "arrived while the page was failed"))
  await flush()
  failGoal = false
  chat.retryCentre("session")
  await flush()
  assert.equal(chat.views.session.status, "ready", "the retry recovered the centre")
  assert.deepEqual(chat.views.session.branches.worker.rows.map(row => row.text), ["on worker", "arrived while the page was failed"], "the side line shows the event it received while the page was failed, once")
})

test("a retry's late policy read does not undo a failure a goal refresh published while it was out", async t => {
  const policyRead = deferred()
  const policy = { preset: "read-only", allowedTools: [], disallowedTools: [] }
  const newer = { preset: "full-access", allowedTools: [], disallowedTools: [] }
  let arm = false
  let goalFails = false
  let policyReads = 0
  const chat = session(t, params => page([input(1, `on ${params.branchId}`)], 1), {
    "session.goal.get": params => {
      if (goalFails) throw new Error("the goal read failed")
      return { branchId: params.branchId, through: 1, goal: null }
    },
    "session.goal.set": () => ({}),
    "session.permission.get": () => {
      if (!arm) return { version: 1, policy }
      policyReads++
      return policyRead.promise
    },
  })
  const supports = globalThis.historyStores.connection.supports
  globalThis.historyStores.connection.supports = name => name === "session.permission.get" || supports(name)
  await flush()
  assert.equal(chat.views.session.status, "ready")
  goalFails = true
  receive(chat, "main", goalInput(2, "goal: carry on"))
  await flush()
  assert.equal(chat.views.session.status, "failed")
  goalFails = false
  arm = true
  chat.retryCentre("session")
  await until(() => policyReads === 1)
  assert.equal(chat.views.session.status, "failed", "the retry reads the policy while the failed page is shown")
  goalFails = true
  const writing = chat.writeGoal("session", "set", "carry on", "main").then(() => "ok", cause => `rejected: ${cause.message}`)
  await flush()
  assert.equal(chat.views.session.status, "failed", "the goal read that crossed the retry fails the page")
  policyRead.resolve({ version: 2, policy: newer })
  await flush()
  assert.equal(chat.views.session.status, "failed", "the retry's late policy read does not undo the failure")
  assert.equal(chat.views.session.error, "the goal read failed")
  assert.deepEqual(chat.views.session.permission, policy, "the policy read that crossed the failure is not shown")
  assert.equal(chat.views.session.permissionVersion, 1)
  assert.equal(await writing, "rejected: the goal read failed")
})

test("a no-op move finishes the page a resync left loading on the line", async t => {
  const modelRead = deferred()
  const oldPage = deferred()
  let armModel = false
  let modelReads = 0
  let rootReads = 0
  const chat = session(t, params => {
    if (params.branchId === "worker") return page([input(1, "on the worker")], 1)
    rootReads++
    return rootReads === 1 ? page([input(1, "on the root")], 1) : oldPage.promise
  }, {
    "session.branch_set_default": () => "worker",
    "session.model.get": params => {
      if (params.branchId === "worker" && armModel && ++modelReads === 1) return modelRead.promise
      return { branchId: params.branchId, through: params.through }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  const reconnecting = chat.resync()
  await until(() => rootReads === 2)
  armModel = true
  const moving = chat.setDefaultBranch("session", "worker")
  await until(() => chat.views.session.defaultBranchId === "worker")
  assert.equal(chat.views.session.status, "loading", "the resync's loading state is on the moved page")
  await chat.setDefaultBranch("session", "worker")
  assert.equal(chat.views.session.status, "ready", "a no-op move finishes the page the resync left loading")
  modelRead.resolve({ branchId: "worker", through: 1 })
  await moving
  oldPage.resolve(page([input(1, "on the root")], 1))
  await reconnecting
  await flush()
  assert.equal(chat.views.session.status, "ready")
  assert.equal(chat.views.session.defaultBranchId, "worker")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on the worker"])
})

test("a return to the centre while another line's history loads cancels that move", async t => {
  const workerPage = deferred()
  const chat = session(t, params => params.branchId === "worker"
    ? workerPage.promise
    : page([input(1, "on the root")], 1), {
    "session.branch_set_default": params => params.branchId,
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  const moving = chat.setDefaultBranch("session", "worker")
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "main", "the worker's history is still loading")
  await chat.setDefaultBranch("session", "main")
  workerPage.resolve(page([input(1, "on the worker")], 1))
  await moving
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "main", "the accepted return to the centre cancels the earlier move")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on the root"])
})

test("a retry that finishes its reads resumes the reply that arrived while they were out", async t => {
  const goalRead = deferred()
  let fail = true
  let arm = false
  const chat = session(t, () => {
    if (fail) throw new Error("the line could not be read")
    return page([input(1, "stored")], 1)
  }, {
    "session.goal.get": params => {
      if (!arm) return { branchId: params.branchId, through: 1, goal: null }
      // The reply arrives after the retry's window and before its ready page,
      // the interval where the failed page queues but consumes nothing.
      receive(chat, "main", input(2, "the last reply"))
      return goalRead.promise
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "failed")
  fail = false
  arm = true
  chat.retryCentre("session")
  await flush()
  assert.equal(chat.views.session.status, "failed", "the retry waits on the goal read it made")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["stored"], "the retry's window is in before its success")
  goalRead.resolve({ branchId: "main", through: 1, goal: null })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["stored", "the last reply"], "the reply that arrived while the retry read the goal is applied when the retry succeeds")
})

test("a retry a move overtakes does not consume the old centre's queue", async t => {
  const goalRead = deferred()
  const workerPage = deferred()
  let fail = true
  let arm = false
  let workerRead = false
  let toolReads = 0
  const chat = session(t, params => {
    if (params.branchId === "worker") {
      workerRead = true
      return workerPage.promise
    }
    if (fail) throw new Error("the line could not be read")
    return page([input(1, "stored")], 1)
  }, {
    "session.branch_set_default": params => params.branchId,
    "session.goal.get": params => arm ? goalRead.promise : { branchId: params.branchId, through: 1, goal: null },
    "session.tool.get": () => {
      toolReads++
      return { requestId: "r1", callId: "call-1", name: "exec", input: {} }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "failed")
  fail = false
  arm = true
  chat.retryCentre("session")
  await flush()
  assert.equal(chat.views.session.status, "failed", "the retry is out on its goal read")
  receive(chat, "main", event(2, { kind: "tool-completed", requestId: "r1", callId: "call-1", result: { content: [], isError: false } }))
  const moving = chat.setDefaultBranch("session", "worker")
  await until(() => workerRead)
  goalRead.resolve({ branchId: "main", through: 1, goal: null })
  await flush()
  assert.equal(toolReads, 0, "a retry the move overtook does not start the old centre's consumer")
  workerPage.resolve(page([input(1, "on the worker")], 1))
  await moving
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "worker")
  assert.equal(chat.views.session.status, "ready")
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on the worker"], "the event the old centre queued does not enter the moved page")
})

test("a resumed live consumer applies each event once", async t => {
  const goalRead = deferred()
  const hydration = deferred()
  let fail = true
  let arm = false
  let toolReads = 0
  const chat = session(t, () => {
    if (fail) throw new Error("the line could not be read")
    return page([input(1, "stored")], 1)
  }, {
    "session.goal.get": params => {
      if (!arm) return { branchId: params.branchId, through: 1, goal: null }
      receive(chat, "main", event(2, { kind: "tool-completed", requestId: "r1", callId: "call-1", result: { content: [], isError: false } }))
      return goalRead.promise
    },
    "session.tool.get": () => {
      toolReads++
      return hydration.promise
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "failed")
  fail = false
  arm = true
  chat.retryCentre("session")
  await flush()
  goalRead.resolve({ branchId: "main", through: 1, goal: null })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  receive(chat, "main", input(3, "while the consumer ran"))
  await until(() => toolReads === 1)
  hydration.resolve({ requestId: "r1", callId: "call-1", name: "exec", input: {} })
  await flush()
  assert.deepEqual(chat.views.session.rows.map(row => row.kind), ["user", "tool", "user"])
  assert.deepEqual(chat.views.session.rows.filter(row => row.kind === "user").map(row => row.text), ["stored", "while the consumer ran"], "each event the resumed consumer held is applied once")
  assert.equal(chat.views.session.rows.filter(row => row.kind === "tool").length, 1)
  assert.equal(toolReads, 1, "a consumer that is already running is not started again for the event it holds")
})

/**
 * Opens a Session whose centre read fails, retries it, and returns with the
 * retry's live consumer suspended on `session.tool.get`: the tool event it
 * replays and anything received later wait in the centre's queue.
 */
async function retrySuspended(t, history, duringGoal = () => {}) {
  const goalRead = deferred()
  const hydration = deferred()
  const state = { arm: false, toolReads: 0 }
  const chat = session(t, history, {
    "session.branch_set_default": params => params.branchId,
    "session.goal.get": params => {
      if (!state.arm) return { branchId: params.branchId, through: 1, goal: null }
      state.arm = false
      receive(chat, "main", event(2, { kind: "tool-completed", requestId: "r1", callId: "call-1", result: { content: [], isError: false } }))
      duringGoal(chat)
      return goalRead.promise
    },
    "session.tool.get": () => {
      state.toolReads++
      return state.toolReads === 1 ? hydration.promise : { requestId: "r1", callId: "call-1", name: "exec", input: {} }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "failed", "the centre starts on a failed page")
  state.arm = true
  chat.retryCentre("session")
  await flush()
  goalRead.resolve({ branchId: "main", through: 1, goal: null })
  await flush()
  assert.equal(chat.views.session.status, "ready", "the retry's reads finish")
  await until(() => state.toolReads === 1)
  return { chat, hydration, state }
}

test("a stale replayed failure does not stop the centre the line returned to", async t => {
  const workerPage = deferred()
  let mainReads = 0
  let workerRead = false
  const { chat, hydration } = await retrySuspended(t, params => {
    if (params.branchId === "worker") {
      workerRead = true
      return workerPage.promise
    }
    if (++mainReads === 1) throw new Error("the line could not be read")
    return page([input(1, "stored")], 1)
  })
  const away = chat.setDefaultBranch("session", "worker")
  await until(() => workerRead)
  workerPage.resolve(page([input(1, "on the worker")], 1))
  await away
  await flush()
  await chat.setDefaultBranch("session", "main")
  await flush()
  assert.equal(chat.views.session.defaultBranchId, "main")
  hydration.reject(new Error("the tool lookup failed"))
  await flush()
  receive(chat, "main", input(4, "after the stale failure"))
  await flush()
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["stored", "after the stale failure"], "the centre the line returned to keeps consuming its live events")
})

test("a stale replayed failure does not stop the side line reopened on the old centre", async t => {
  const workerPage = deferred()
  let mainReads = 0
  let workerRead = false
  const { chat, hydration } = await retrySuspended(t, params => {
    if (params.branchId === "worker") {
      workerRead = true
      return workerPage.promise
    }
    if (++mainReads === 1) throw new Error("the line could not be read")
    return page([input(1, "stored")], 1)
  })
  const away = chat.setDefaultBranch("session", "worker")
  await until(() => workerRead)
  workerPage.resolve(page([input(1, "on the worker")], 1))
  await away
  await flush()
  chat.ensureLine("session", "main")
  await until(() => chat.views.session.branches["main"]?.status === "ready")
  assert.equal(chat.views.session.branches["main"].status, "ready", "the reopened line reads its own window")
  hydration.reject(new Error("the tool lookup failed"))
  await flush()
  assert.equal(chat.views.session.branches["main"].status, "ready", "the reopened line keeps the state its own read published")
  receive(chat, "main", input(4, "on the reopened line"))
  await flush()
  assert.deepEqual(chat.views.session.branches["main"].rows.map(row => row.text), ["stored", "on the reopened line"], "the reopened line keeps consuming its live events")
})

test("a stale replayed failure does not stop the centre after an in-place reload", async t => {
  const workerPage = deferred()
  const reload = deferred()
  let mainReads = 0
  const { chat, hydration } = await retrySuspended(t, params => {
    if (params.branchId === "worker") return workerPage.promise
    mainReads++
    if (mainReads === 1) throw new Error("the line could not be read")
    if (mainReads === 3) return reload.promise
    return page([input(1, "stored")], 1)
  })
  const reconnecting = chat.resync()
  await until(() => mainReads === 3)
  hydration.reject(new Error("the tool lookup failed"))
  await flush()
  reload.resolve(page([input(1, "stored")], 1))
  await reconnecting
  await flush()
  assert.equal(chat.views.session.status, "ready")
  receive(chat, "main", input(4, "after the reload"))
  await flush()
  assert.deepEqual(chat.views.session.rows.filter(row => row.kind === "user").map(row => row.text), ["stored", "after the reload"], "the reloaded centre keeps consuming its live events")
})

test("a stale live consumer that finishes late does not write into the stream that replaced it", async t => {
  const workerPage = deferred()
  let mainReads = 0
  let workerRead = false
  const { chat, hydration } = await retrySuspended(t, params => {
    if (params.branchId === "worker") {
      workerRead = true
      return workerPage.promise
    }
    if (++mainReads === 1) throw new Error("the line could not be read")
    return page([input(1, "stored")], 1)
  }, chat => receive(chat, "main", input(3, "while the retry read")))
  const away = chat.setDefaultBranch("session", "worker")
  await until(() => workerRead)
  workerPage.resolve(page([input(1, "on the worker")], 1))
  await away
  await flush()
  chat.ensureLine("session", "main")
  await until(() => chat.views.session.branches["main"]?.status === "ready")
  hydration.resolve({ requestId: "r1", callId: "call-1", name: "exec", input: {} })
  await flush()
  assert.deepEqual(chat.views.session.branches["main"].rows.map(row => row.text), ["stored"], "the replaced stream keeps its own window")
  receive(chat, "main", input(4, "after the handover"))
  await flush()
  assert.deepEqual(chat.views.session.branches["main"].rows.map(row => row.text), ["stored", "after the handover"], "the replaced stream consumes its own events once")
})

test("a goal read from before a move away does not fail the centre that came back", async t => {
  const workerPage = deferred()
  const goalRead = deferred()
  let goalArmed = false
  let workerRead = false
  const chat = session(t, params => {
    if (params.branchId === "worker") {
      workerRead = true
      return workerPage.promise
    }
    return page([input(1, "stored")], 1)
  }, {
    "session.branch_set_default": params => params.branchId,
    "session.goal.set": () => undefined,
    "session.goal.get": params => {
      if (goalArmed && params.branchId === "main") {
        goalArmed = false
        return goalRead.promise
      }
      return { branchId: params.branchId, through: 1, goal: null }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  goalArmed = true
  const writing = chat.writeGoal("session", "set", "on the main line")
  writing.catch(() => {})
  await until(() => !goalArmed)
  assert.equal(chat.views.session.status, "ready", "the goal read is out on the ready page")
  const away = chat.setDefaultBranch("session", "worker")
  await until(() => workerRead)
  workerPage.resolve(page([input(1, "on the worker")], 1))
  await until(() => chat.views.session.defaultBranchId === "worker")
  const back = chat.setDefaultBranch("session", "main")
  await until(() => chat.views.session.defaultBranchId === "main")
  goalRead.reject(new Error("the goal read failed"))
  await flush()
  await away
  await back
  assert.equal(chat.views.session.status, "ready", "a goal read from before the centre moved away does not fail the centre that came back")
  assert.equal(chat.views.session.error, "")
})

test("a goal read the next move overtook does not put the ready page back to loading", async t => {
  const workerPage = deferred()
  const goalRead = deferred()
  let goalArmed = false
  let workerRead = false
  const chat = session(t, params => {
    if (params.branchId === "worker") {
      workerRead = true
      return workerPage.promise
    }
    return page([input(1, "stored")], 1)
  }, {
    "session.branch_set_default": params => params.branchId,
    "session.goal.get": params => {
      if (goalArmed && params.branchId === "worker") {
        goalArmed = false
        return goalRead.promise
      }
      return { branchId: params.branchId, through: 1, goal: null }
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  goalArmed = true
  const moving = chat.setDefaultBranch("session", "worker")
  await until(() => workerRead)
  workerPage.resolve(page([input(1, "on the worker")], 1))
  await until(() => chat.views.session.defaultBranchId === "worker")
  await until(() => !goalArmed)
  assert.equal(chat.views.session.status, "ready", "the move's goal read is out on the ready page")
  await chat.setDefaultBranch("session", "worker")
  goalRead.reject(new ConnectionLost("the connection went away"))
  await flush()
  await moving
  assert.equal(chat.views.session.status, "ready", "a goal read the next move overtook does not put the ready page back to loading")
})

test("a resync failure a move overtook does not fail the centre the move installed", async t => {
  const sideQuestions = deferred()
  let arm = false
  let held = false
  let directory = "main"
  const chat = session(t, params => page([input(1, `on ${params.branchId}`)], 1), {
    "session.branches": () => ({
      branches: [
        { branchId: "worker", parentBranchId: "main", branchPoint: 0, head: 0, state: "open" },
        { branchId: "side", parentBranchId: "worker", branchPoint: 0, head: 0, state: "open" },
      ],
      defaultBranchId: directory,
    }),
    "session.question.pending": params => {
      if (arm && params.branchId === "side") {
        held = true
        return sideQuestions.promise
      }
      return { ...params, asks: [], next: null }
    },
    "session.branch_set_default": () => {
      directory = "worker"
      return "worker"
    },
  })
  await flush()
  assert.equal(chat.views.session.status, "ready")
  arm = true
  const reconnecting = chat.resync()
  await until(() => held)
  assert.equal(chat.views.session.status, "loading", "the resync holds the page while it reads an unopened side line")
  await chat.setDefaultBranch("session", "worker")
  assert.equal(chat.views.session.defaultBranchId, "worker")
  assert.equal(chat.views.session.status, "ready")
  sideQuestions.reject(new Error("unknown branch"))
  await reconnecting
  await flush()
  assert.equal(chat.views.session.status, "ready", "the resync's late failure does not fail the centre the move installed")
  assert.equal(chat.views.session.error, "")
  receive(chat, "worker", input(2, "on the worker live"))
  await flush()
  assert.deepEqual(chat.views.session.rows.map(row => row.text), ["on worker", "on the worker live"], "the moved centre keeps consuming its live events")
})
