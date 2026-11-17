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

const { ProtocolError, readAccountProviders, readBranchCreate, readHistory, readDisplayHistory, readMemberApproval, readNotification, readPermissionState, readProviderCatalog } = await import("./read.ts")

test("permission replies ignore removed policy keys and still validate presets and tool lists", () => {
  const policy = { preset: "full-access", allowedTools: ["read"], disallowedTools: ["exec"] }
  assert.deepEqual(readPermissionState({ version: 7, policy: { ...policy, fileAccess: "read-only", toolApproval: "ask" } }), { version: 7, policy })
  assert.throws(() => readPermissionState({ version: 7, policy: { ...policy, preset: "invalid" } }), ProtocolError)
  assert.throws(() => readPermissionState({ version: 7, policy: { ...policy, disallowedTools: [false] } }), ProtocolError)
})

test("model discovery reads managed providers and distinguishes missing catalogs from failures", () => {
  assert.deepEqual(readAccountProviders([
    { providerId: "dim", displayName: "Dim account", loginMethods: ["browser"] },
    { providerId: "other", displayName: "Other account", loginMethods: [] },
  ]), [
    { providerId: "dim", displayName: "Dim account" },
    { providerId: "other", displayName: "Other account" },
  ])
  assert.equal(readProviderCatalog({ catalog: null }), undefined)
  assert.deepEqual(readProviderCatalog({ catalog: {
    models: [
      { id: "ready", name: "Ready", unavailableReason: null },
      { id: "blocked", name: "Blocked", unavailableReason: "provider_unavailable" },
    ],
    disabledModelIds: ["ready"],
  } }), {
    models: [
      { id: "ready", name: "Ready" },
      { id: "blocked", name: "Blocked", unavailableReason: "provider_unavailable" },
    ],
    disabledModelIds: ["ready"],
  })
  assert.throws(() => readAccountProviders({ providers: [] }), ProtocolError)
  assert.throws(() => readAccountProviders([{ providerId: "dim" }]), ProtocolError)
  assert.throws(() => readProviderCatalog({ catalog: { models: [] } }), ProtocolError)
})

function page(event) {
  return readHistory({ events: [event], through: event.sequence, nextAfter: event.sequence, done: true })
}

function event(type, name, data, sequence = 1) {
  return { id: `e${sequence}`, sequence, create_at: 1_000 + sequence, type, name, data }
}

test("branch facts are decoded with their line and cut", () => {
  const summary = event("display", "compaction", { strategy: "summary" }, 2)
  const display = readDisplayHistory({ items: [summary], through: 2, nextAfter: 2, nextBefore: 2, done: true })
  assert.deepEqual(display.events[0], { id: "e2", sequence: 2, createAt: 1002, content: { kind: "other" } })
  assert.deepEqual(readNotification("session.display", { sessionId: "s", branchId: "main", item: summary }), {
    method: "session.display", sessionId: "s", branchId: "main", event: display.events[0],
  })
  assert.throws(() => readDisplayHistory({ events: [summary], through: 2, nextAfter: 2, done: true }), ProtocolError)
  const created = page(event("branch", "create", {
    at: { kind: "sequence", sequence: 3 },
    source: { kind: "host", operation_id: "op-1" },
    branch: "b-7c",
    prompt: "parser race",
  }))
  assert.deepEqual(created.events[0].content, {
    kind: "branch-create",
    branchId: "b-7c",
    at: 3,
    prompt: "parser race",
    source: { kind: "host", operationId: "op-1" },
  })

  const ended = page(event("branch", "ended", {
    branch: "b-7c",
    endEvent: 4,
    source: { kind: "tool", turn_id: "t", request_id: "r", call_id: "c" },
    ancestor: "b-02",
  }))
  assert.deepEqual(ended.events[0].content, {
    kind: "branch-ended",
    branchId: "b-7c",
    source: { kind: "tool", turnId: "t", requestId: "r", callId: "c" },
    ancestor: "b-02",
  })
})

test("raw branch messages and input deletions are decoded", () => {
  const message = page(event("input", "message", {
    blocks: [{ type: "raw", text: "<dim-branch-message from=\"main\">\nhello\n</dim-branch-message>" }],
    mode: "steer",
  }))
  assert.deepEqual(message.events[0].content, {
    kind: "source-input",
    text: "<dim-branch-message from=\"main\">\nhello\n</dim-branch-message>",
    blocks: [{ text: "<dim-branch-message from=\"main\">\nhello\n</dim-branch-message>", retire: false }],
    mode: "steer",
  })

  const deleted = page(event("input", "deleted", { input_id: "e9" }, 2))
  assert.deepEqual(deleted.events[0].content, { kind: "input-deleted", inputId: "e9" })
})

test("branch_create replies and malformed branch facts are read strictly", () => {
  assert.deepEqual(readBranchCreate({
    branch: { branchId: "b-7c", parentBranchId: "main", branchPoint: 3, createEvent: 8 },
  }), { branchId: "b-7c", parentBranchId: "main", branchPoint: 3, createEvent: 8 })

  assert.throws(
    () => page(event("branch", "create", {
      at: { kind: "sequence", sequence: 3 },
      source: { kind: "unknown" },
      branch: "b-7c",
      prompt: "parser race",
    })),
    ProtocolError,
  )
})


test("member approvals preserve the original action and retire by branch and route", () => {
  const detail = { state: "pending", approvalId: "route", descriptor: {
    delegationId: "delegation", member: { agentId: "reviewer", sessionId: "member", branchId: "main" },
    requestId: "original", call: { name: "exec", input: { command: "cargo test", nested: { literal: "</script>" } } }, reason: "Member action",
  } }
  assert.deepEqual(readMemberApproval(detail), detail)
  assert.deepEqual(readMemberApproval({ state: "ended" }), { state: "ended" })
  assert.throws(() => readMemberApproval({ state: "pending", approvalId: "route" }), ProtocolError)
  assert.throws(() => readMemberApproval({ ...detail, descriptor: { ...detail.descriptor, call: { name: "exec", input: null } } }), ProtocolError)
  assert.deepEqual(readNotification("session.notice", { sessionId: "leader", source: "Collaboration", kind: "memberApprovalEnded", data: { branchId: "review", approvalId: "route" } }), {
    method: "memberApprovalEnded", sessionId: "leader", branchId: "review", approvalId: "route",
  })
  assert.deepEqual(readNotification("approval.resolved", { sessionId: "leader", branchId: "review", requestId: "outer", decision: "deny" }), {
    method: "approval.resolved", sessionId: "leader", branchId: "review", requestId: "outer",
  })
  assert.equal(readNotification("session.notice", { sessionId: "leader", source: "AnotherOwner", kind: "memberApprovalEnded", data: {} }), undefined)
})

test("model progress reads the open thinking block's producer start", () => {
  assert.deepEqual(readNotification("session.model.progress", {
    sessionId: "s", branchId: "main", requestId: "r1",
    text: "", textOffset: 0, thinking: "weigh", thinkingOffset: 0, thinkingStartedAt: 1_002,
  }), {
    method: "session.model.progress",
    progress: { sessionId: "s", branchId: "main", requestId: "r1", text: "", textOffset: 0, thinking: "weigh", thinkingOffset: 0, thinkingStartedAt: 1_002 },
  })
  assert.equal(readNotification("session.model.progress", {
    sessionId: "s", branchId: "main", text: "", textOffset: 0, thinking: "", thinkingOffset: 0,
  }).progress.thinkingStartedAt, undefined)
})

test("tool-use-start opens a call as its own output event", () => {
  const started = page(event("step", "tool-use-start", {
    turn_id: "turn", request_id: "request", call_id: "call", name: "echo",
  }))
  assert.deepEqual(started.events[0].content, { kind: "tool-use-start", requestId: "request", callId: "call", name: "echo" })
})

test("a loaded Skill block is read as a note, not as the operator's message", () => {
  const block = '<dim-skill name="checkin">\nSkill: checkin\nDescription: D\nFile: /x/SKILL.md\n\nInstructions:\nBODY\n</dim-skill>'
  const loaded = page(event("input", "message", {
    user: { content: [
      { type: "text", text: "check the build" },
      { type: "text", text: block },
    ] },
    mode: "queue",
  }))
  assert.equal(loaded.events[0].content.text, "check the build")
  assert.deepEqual(loaded.events[0].content.skills, ["checkin"])
  assert.equal(loaded.events[0].content.user.content.length, 2, "the raw content is kept")

  const typed = page(event("input", "message", {
    user: { content: [{ type: "text", text: '<dim-skill name="mine">as typed' }] },
    mode: "queue",
  }, 2))
  assert.equal(typed.events[0].content.text, '<dim-skill name="mine">as typed')
  assert.equal(typed.events[0].content.skills, undefined)
})

test("a prompt hook's context is counted, not read as the operator's message", () => {
  const hooks = [
    '<dim-hooks kind="prompt-submit">\nSEARCH-MEMORY-FIRST\n</dim-hooks>',
    '<dim-hooks kind="prompt-submit">\nread the briefing again\n</dim-hooks>',
  ]
  const hooked = page(event("input", "message", {
    user: { content: [
      { type: "text", text: "why does the parser stall?" },
      ...hooks.map((text) => ({ type: "text", text })),
    ] },
    mode: "queue",
  }))
  assert.equal(hooked.events[0].content.text, "why does the parser stall?")
  assert.equal(hooked.events[0].content.hooks, 2)
  assert.equal(hooked.events[0].content.skills, undefined)
  assert.equal(hooked.events[0].content.user.content.length, 3, "the raw content is kept")

  // The first block is the operator's, whatever it opens, and a longer name is
  // not the element it starts with.
  for (const typed of [
    '<dim-hooks kind="prompt-submit">typed by hand</dim-hooks>',
    '<dim-hooksx kind="prompt-submit">no such element</dim-hooksx>',
  ]) {
    const read = page(event("input", "message", {
      user: { content: [{ type: "text", text: typed }] },
      mode: "queue",
    }, 3))
    assert.equal(read.events[0].content.text, typed, typed)
    assert.equal(read.events[0].content.hooks, undefined, typed)
  }
})
