import assert from "node:assert/strict"
import { registerHooks } from "node:module"
import test from "node:test"

registerHooks({
  resolve(specifier, context, next) {
    try {
      return next(specifier, context)
    } catch (error) {
      if ((specifier.startsWith("./") || specifier.startsWith("../")) && !specifier.endsWith(".ts")) {
        return next(`${specifier}.ts`, context)
      }
      throw error
    }
  },
})

const { Lines } = await import("./lines.ts")
const { approvalKeyDecision, approvalKeyboardBranch, canContinueFrom, lineMarks } = await import("./features/lines/line-state.ts")
const en = (await import("./i18n/en.ts")).default
const zhCN = (await import("./i18n/zh-CN.ts")).default

function event(sequence, content) {
  return { id: `e${sequence}`, sequence, createAt: 1_000 + sequence, content }
}

function create(sequence, branchId, at, prompt = branchId) {
  return event(sequence, {
    kind: "branch-create",
    branchId,
    at,
    prompt,
    source: { kind: "host", operationId: `op-${branchId}` },
  })
}

function user(sequence) {
  return event(sequence, { kind: "input", text: `message ${sequence}`, content: [{ type: "text", text: `message ${sequence}` }] })
}

test("lines use owner details for inherited message counts and keep tree order", () => {
  const lines = new Lines()
  lines.apply("main", user(2))
  lines.apply("main", create(3, "b-child", 4, "parser race\nmore"))
  lines.apply("main", user(5))
  lines.apply("b-child", user(6))
  lines.apply("b-child", create(7, "b-grandchild", 6, "the guard"))

  lines.detail({ branchId: "b-child", parentBranchId: "main", branchPoint: 4, state: "open", head: 7, purpose: "parser race\nmore", cutMessages: 1 })
  lines.detail({ branchId: "b-grandchild", parentBranchId: "b-child", branchPoint: 6, state: "open", head: 1, cutMessages: 2 })
  const views = lines.views()
  assert.deepEqual(views.map((line) => [line.id, line.depth]), [
    ["main", 0],
    ["b-child", 1],
    ["b-grandchild", 2],
  ])
  assert.equal(views[1].purpose, "parser race")
  assert.deepEqual(views[1].cut, { sequence: 4, messages: 1 })
  assert.deepEqual(views[2].cut, { sequence: 6, messages: 2 })
})

test("creation failures and every end source map to the line states", () => {
  const lines = new Lines()
  lines.apply("main", create(1, "b-tool", 1))
  lines.apply("main", create(2, "b-host", 1))
  lines.apply("main", create(3, "b-child", 1))
  lines.apply("main", create(4, "b-failed", 1))
  lines.apply("main", {
    id: "e5",
    sequence: 5,
    createAt: 1_005,
    content: { kind: "branch-create-failed", branchId: "b-failed", status: "failed", error: "boom" },
  })
  lines.apply("main", {
    id: "e6",
    sequence: 6,
    createAt: 1_006,
    content: { kind: "branch-ended", branchId: "b-tool", source: { kind: "tool", turnId: "t", requestId: "r", callId: "c" } },
  })
  lines.apply("main", {
    id: "e7",
    sequence: 7,
    createAt: 1_007,
    content: { kind: "branch-ended", branchId: "b-host", source: { kind: "host", operationId: "op" } },
  })
  lines.apply("main", {
    id: "e8",
    sequence: 8,
    createAt: 1_008,
    content: {
      kind: "branch-ended",
      branchId: "b-child",
      ancestor: "b-tool",
      source: { kind: "host", operationId: "op" },
    },
  })

  const states = Object.fromEntries(lines.views().map((line) => [line.id, line.state]))
  assert.deepEqual(states["b-tool"], { kind: "ended-itself" })
  assert.deepEqual(states["b-host"], { kind: "ended-by-operation" })
  assert.deepEqual(states["b-child"], { kind: "ended-with", ancestor: "b-tool" })
  assert.deepEqual(states["b-failed"], { kind: "creation-failed" })
})

test("directory does not invent a cut count from a partial transcript", () => {
  const lines = new Lines()
  lines.directory([{ branchId: "child", parentBranchId: "main", branchPoint: 1000, state: "open", head: 200 }])
  lines.apply("main", user(999))
  assert.deepEqual(lines.views()[1].cut, { sequence: 1000, messages: undefined })
  lines.detail({ branchId: "child", parentBranchId: "main", branchPoint: 1000, state: "open", head: 200, cutMessages: 40 })
  assert.equal(lines.views()[1].cut.messages, 40)
})

test("the tree marks the root and the default branch, and offers continue from here elsewhere", () => {
  assert.deepEqual(lineMarks("main", "main"), ["main-line", "start"])
  assert.deepEqual(lineMarks("b-child", "b-child"), ["main-line"])
  assert.deepEqual(lineMarks("main", "b-child"), ["start"])
  assert.deepEqual(lineMarks("b-grandchild", "b-child"), [])
  assert.equal(en["lines.mainLine"], "Main line")
  assert.equal(en["lines.start"], "Start")
  assert.equal(zhCN["lines.mainLine"], "主线")
  assert.equal(zhCN["lines.start"], "起点")
  assert.equal(en["lines.continueFrom"], "Continue from here")
  assert.equal(zhCN["lines.continueFrom"], "从此继续")
  // The method decides the refusals; the tree only leaves out a move to the
  // line already holding the role and a line that never opened.
  assert.equal(canContinueFrom({ id: "b-child", state: { kind: "waiting" } }, "main"), true)
  assert.equal(canContinueFrom({ id: "main", state: { kind: "main" } }, "main"), false)
  assert.equal(canContinueFrom({ id: "b-child", state: { kind: "creation-failed" } }, "main"), false)
})

test("approval shortcuts follow the visible surface across wide, expanded and covered layouts", () => {
  const mainOnly = [{ branchId: "main" }]
  const mainAndLine = [{ branchId: "main" }, { branchId: "b-child" }]

  function dispatch(approvals, layout, key, extra = {}) {
    const owner = approvalKeyboardBranch(approvals, layout)
    const answered = []
    for (const branchId of ["main", "b-child"]) {
      if (!approvals.some((request) => request.branchId === branchId)) continue
      const decision = approvalKeyDecision({ key, ...extra }, { keyboard: owner === branchId })
      if (decision !== undefined) answered.push([branchId, decision])
    }
    return answered
  }

  assert.deepEqual(
    dispatch(mainOnly, { detailOpen: false, detailExpanded: false, shellWidth: 1280, defaultBranchId: "main" }, "Enter"),
    [["main", "allow"]],
  )
  assert.deepEqual(
    dispatch(mainOnly, { detailOpen: true, detailExpanded: true, shellWidth: 1280, defaultBranchId: "main", activeLine: "b-child" }, "Enter"),
    [],
  )
  assert.deepEqual(
    dispatch(mainOnly, { detailOpen: true, detailExpanded: false, shellWidth: 680, defaultBranchId: "main", activeLine: "b-child" }, "y"),
    [],
  )
  assert.deepEqual(
    dispatch(mainOnly, { detailOpen: true, detailExpanded: false, shellWidth: 960, defaultBranchId: "main", activeLine: undefined }, "Escape"),
    [],
  )

  assert.deepEqual(
    dispatch(mainAndLine, { detailOpen: true, detailExpanded: true, shellWidth: 1280, defaultBranchId: "main", activeLine: "b-child" }, "Enter"),
    [["b-child", "allow"]],
  )
  assert.deepEqual(
    dispatch(mainAndLine, { detailOpen: true, detailExpanded: false, shellWidth: 680, defaultBranchId: "main", activeLine: "b-child" }, "Escape"),
    [["b-child", "deny"]],
  )
  assert.deepEqual(
    dispatch(mainAndLine, { detailOpen: true, detailExpanded: false, shellWidth: 960, activeLine: "b-child" }, "n"),
    [["b-child", "deny"]],
  )
  assert.deepEqual(
    dispatch(mainAndLine, { detailOpen: true, detailExpanded: false, shellWidth: 1280, defaultBranchId: "main", activeLine: "b-child", detailFocused: true }, "y"),
    [["b-child", "allow"]],
  )
  assert.deepEqual(
    dispatch(mainAndLine, { detailOpen: true, detailExpanded: false, shellWidth: 1280, defaultBranchId: "main", activeLine: "b-child", detailFocused: false }, "Enter"),
    [["main", "allow"]],
  )
  assert.deepEqual(
    dispatch(mainAndLine, { detailOpen: true, detailExpanded: true, shellWidth: 1280, defaultBranchId: "main", activeLine: "b-child" }, "Enter", { target: { tagName: "TEXTAREA" } }),
    [],
  )
})
