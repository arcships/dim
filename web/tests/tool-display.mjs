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

const { diffLines } = await import("../src/features/chat/tool-display.ts")

test("an edit that carried replacements shows every one of them in the order they applied", () => {
  const lines = diffLines({ path: "src/lib.rs", edits: [
    { oldText: "one\nkeep", newText: "first\nkeep" },
    { oldText: "two", newText: "second" },
  ] }, undefined)
  assert.deepEqual(lines, [
    { sign: "-", text: "one" },
    { sign: "-", text: "keep" },
    { sign: "+", text: "first" },
    { sign: "+", text: "keep" },
    { sign: "-", text: "two" },
    { sign: "+", text: "second" },
  ])
})

test("a recorded edit that carried one pair still shows its two sides", () => {
  const lines = diffLines({ path: "src/lib.rs", oldText: "before", newText: "after" }, undefined)
  assert.deepEqual(lines, [
    { sign: "-", text: "before" },
    { sign: "+", text: "after" },
  ])
})

test("the tool's diff wins when it reported one", () => {
  const lines = diffLines({ edits: [{ oldText: "ignored", newText: "ignored" }] }, "--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new")
  assert.deepEqual(lines, [
    { sign: "-", text: "old" },
    { sign: "+", text: "new" },
  ])
})
