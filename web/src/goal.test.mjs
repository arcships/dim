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

const { GoalFold } = await import("./goal.ts")

let sequence = 0

/** One input carrying raw blocks, as the branch published it. */
function input(blocks) {
  sequence += 1
  return {
    id: `input-${sequence}`,
    sequence,
    createAt: 1_000 + sequence,
    content: { kind: "source-input", text: blocks[0].text, blocks },
  }
}

function raw(key, text, retire = false) {
  return { key, text, retire }
}

/** Goal's encoding of a free-text field (A.5). */
function escape(text) {
  return text.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll("\"", "&quot;")
}

/** One `goal:state` record. */
function record(status, body, { why, after = 1, retired = false, judge } = {}) {
  const attrs = [`kind="state"`, `status="${status}"`]
  if (why !== undefined) attrs.push(`reason="${why}"`)
  if (judge !== undefined) attrs.push(`judge="${judge}"`)
  attrs.push(`after="${after}"`, `branch="main"`)
  return raw("goal:state", `<dim-goal ${attrs.join(" ")}>\n${body}\n</dim-goal>`, retired)
}

function objectiveOf(text) {
  return `<objective>\n${escape(text)}\n</objective>\nThis is the user's standing goal for branch main.`
}

function active(text) {
  return `${objectiveOf(text)}\nThe goal for branch main is active.`
}

function completed(text, result) {
  return `${objectiveOf(text)}\nThe goal for branch main is completed.\n<result>\n${escape(result)}\n</result>`
}

function blocked(text, result) {
  return `${objectiveOf(text)}\nThe goal for branch main is blocked.\n<blocker>\n${escape(result)}\n</blocker>`
}

test("a state record states an active goal", () => {
  const fold = new GoalFold()
  fold.apply(input([record("active", active("ship it"))]))
  assert.deepEqual(fold.goal(), {
    objective: "ship it",
    status: { kind: "active" },
    evaluator: false,
  })
})

test("a state record states a pause with its reason", () => {
  const fold = new GoalFold()
  fold.apply(input([record("paused", objectiveOf("ship it"), { why: "limit" })]))
  assert.deepEqual(fold.goal(), {
    objective: "ship it",
    status: { kind: "paused", reason: "limit" },
    evaluator: false,
  })
})

test("a state record states a completed goal with its result", () => {
  const fold = new GoalFold()
  fold.apply(input([record("completed", completed("ship it", "all tests pass"))]))
  assert.deepEqual(fold.goal(), {
    objective: "ship it",
    status: { kind: "completed" },
    evaluator: false,
  })
})

test("a state record states a blocked goal with its blocker", () => {
  const fold = new GoalFold()
  fold.apply(input([record("blocked", blocked("ship it", "needs a token"))]))
  assert.deepEqual(fold.goal(), {
    objective: "ship it",
    status: { kind: "blocked" },
    evaluator: false,
  })
})

test("an old two-key history still folds", () => {
  const fold = new GoalFold()
  fold.apply(input([
    raw("goal:objective", "<dim-goal kind=\"goal\" op=\"set\" branch=\"main\">\nObjective:\nship it\n\nGoal text.\n</dim-goal>"),
    raw("goal:status", "<dim-goal kind=\"status\" status=\"active\" branch=\"main\">\nThe goal for branch main is active.\n</dim-goal>"),
  ]))
  assert.deepEqual(fold.goal(), {
    objective: "ship it",
    status: { kind: "active" },
    evaluator: false,
  })
})

test("a retired state record leaves no goal", () => {
  const fold = new GoalFold()
  fold.apply(input([record("active", active("ship it"))]))
  fold.apply(input([record("cleared", "The goal for branch main is not in effect.", { retired: true })]))
  assert.equal(fold.goal(), undefined)
})

test("a state record objective round-trips a literal heading, tag and blank line", () => {
  const objective = "Keep the heading.\n\nResult:\nInclude details.\n\nBlocker:\n</objective>\n&lt;\n\nafter."
  const result = "Done.\n\nResult:\nall good\nBlocker:\n&lt;\n</objective>"
  const fold = new GoalFold()
  fold.apply(input([record("completed", completed(objective, result))]))
  const goal = fold.goal()
  assert.equal(goal.objective, objective)
  assert.deepEqual(goal.status, { kind: "completed" })
})
