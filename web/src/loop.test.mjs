import assert from "node:assert/strict"
import { registerHooks } from "node:module"
import test from "node:test"

registerHooks({ resolve(specifier, context, next) {
  try { return next(specifier, context) } catch (error) {
    if (specifier.startsWith(".") && !specifier.endsWith(".ts")) return next(`${specifier}.ts`, context)
    throw error
  }
} })
const { intervalSeconds, loopReport, parseInterval, parseLoopRequest, readLoop } = await import("./loop.ts")

test("a bare /loop shows the branch's loops", () => {
  assert.deepEqual(parseLoopRequest(""), { form: "show" })
  assert.deepEqual(parseLoopRequest("   "), { form: "show" })
})

test("a prompt alone starts a self-paced loop", () => {
  assert.deepEqual(parseLoopRequest("check the deploy"), {
    form: "start",
    prompt: "check the deploy",
  })
})

test("an interval counts only as the first token and only when it parses", () => {
  assert.deepEqual(parseLoopRequest("5m check the deploy"), {
    form: "start",
    prompt: "check the deploy",
    interval: "5m",
  })
  assert.deepEqual(parseLoopRequest("30s watch"), { form: "start", prompt: "watch", interval: "30s" })
  assert.deepEqual(parseLoopRequest("2h triage"), { form: "start", prompt: "triage", interval: "2h" })
  assert.deepEqual(parseLoopRequest("1d report"), { form: "start", prompt: "report", interval: "1d" })

  // `7m` parses, so it is an interval; the server rounds it up to whole
  // minutes and tells the model what it picked.
  assert.deepEqual(parseLoopRequest("7m the deploy"), {
    form: "start",
    prompt: "the deploy",
    interval: "7m",
  })

  // Not an interval: the whole line is the prompt.
  assert.deepEqual(parseLoopRequest("check 5m later"), { form: "start", prompt: "check 5m later" })
  assert.deepEqual(parseLoopRequest("0m nothing"), { form: "start", prompt: "0m nothing" })
  assert.deepEqual(parseLoopRequest("5x watch"), { form: "start", prompt: "5x watch" })
})

test("stop names one loop or the branch's only one", () => {
  assert.deepEqual(parseLoopRequest("stop"), { form: "stop" })
  assert.deepEqual(parseLoopRequest("stop 7f2"), { form: "stop", loopId: "7f2" })
  assert.equal(parseLoopRequest("stop two words"), undefined)
  assert.equal(parseInterval("0s"), undefined)
  assert.equal(parseInterval("5m"), "5m")
})

test("the interval also reads as whole seconds for the wire", () => {
  assert.equal(intervalSeconds("30s"), 30)
  assert.equal(intervalSeconds("5m"), 300)
  assert.equal(intervalSeconds("2h"), 7200)
  assert.equal(intervalSeconds("1d"), 86400)
  assert.equal(intervalSeconds("0m"), undefined)
  assert.equal(intervalSeconds("5x"), undefined)
})

test("the report is the same line the terminal client prints", () => {
  assert.deepEqual(loopReport([]), ["No loop on this line."])
  const loop = readLoop({ loopId: "7f2", state: "running", round: 3, nextAt: "2026-11-13T09:41:00.000Z", prompt: "check" })
  assert.deepEqual(loopReport([loop]), ["7f2 running round 3 next 2026-11-13T09:41:00.000Z check"])
})

test("a wire value without an id is not a loop", () => {
  assert.equal(readLoop({}), undefined)
  assert.equal(readLoop({ loopId: "" }), undefined)
  assert.equal(readLoop(null), undefined)
  assert.deepEqual(readLoop({ loopId: "a" }), { loopId: "a", state: "unknown", round: 0, nextAt: null, prompt: "" })
})
