import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { registerHooks, stripTypeScriptTypes } from "node:module"
import test from "node:test"

registerHooks({
  load(url, context, next) {
    if (url.endsWith(".ts")) return { format: "module", source: stripTypeScriptTypes(readFileSync(new URL(url), "utf8"), { mode: "transform" }), shortCircuit: true }
    return next(url, context)
  },
})
const { sortKey } = await import("../src/components/sidebar-sort.ts")

const session = { createdAt: 100, updatedAt: 900 }

test("recent activity orders by the last update", () => {
  assert.equal(sortKey(session, "recentActive"), 900)
})

test("recent creation orders by when the session began", () => {
  assert.equal(sortKey(session, "recentCreated"), 100)
})
