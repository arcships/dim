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

const { failureDetails, failureLine, retryAnnouncement, retrySeconds, retryStatus, FAILURE_KINDS } = await import("./failures.ts")
const { i18n } = await import("./i18n/index.ts")
const { default: en } = await import("./i18n/en.ts")
const { default: zhCN } = await import("./i18n/zh-CN.ts")

i18n.global.setLocaleMessage("zh-CN", zhCN)

function failure(fields = {}) {
  return {
    kind: "server_error",
    status: 503,
    providerCode: "overloaded",
    requestId: "req_1",
    reason: "The server is overloaded",
    cause: ["upstream 503"],
    attempts: 3,
    waitedMs: 4200,
    retryAfterMs: 60_000,
    ...fields,
  }
}

function inLocale(locale, body) {
  i18n.global.locale.value = locale
  try {
    body()
  } finally {
    i18n.global.locale.value = "en"
  }
}

test("a failure is one line with no prefix", () => {
  assert.equal(
    failureLine(failure(), false),
    "The provider reported a server error: The server is overloaded. Try again later or choose another model. Tried 3 times over 4 s.",
  )
})

test("a failure line is worded in Chinese", () => {
  inLocale("zh-CN", () => {
    assert.equal(
      failureLine(failure(), false),
      "提供方报告了服务器错误: The server is overloaded. 稍后重试或换一个模型。 已尝试 3 次，历时 4 秒。",
    )
  })
})

test("a failure after output starts with the incomplete sentence", () => {
  assert.equal(
    failureLine(failure(), true),
    "The response above is incomplete. The provider reported a server error: The server is overloaded. Try again later or choose another model. Tried 3 times over 4 s.",
  )
})

test("a single attempt has no tried suffix", () => {
  assert.equal(
    failureLine(failure({ attempts: 1, waitedMs: 0 }), false),
    "The provider reported a server error: The server is overloaded. Try again later or choose another model.",
  )
})

test("every kind has a row in both tables", () => {
  assert.equal(FAILURE_KINDS.length, 12)
  for (const kind of FAILURE_KINDS) {
    for (const part of ["meaning", "action"]) {
      const key = `failure.kind.${kind}.${part}`
      for (const table of [en, zhCN]) {
        assert.equal(typeof table[key], "string", key)
        assert.ok(table[key].length > 0, key)
      }
    }
  }
})

test("the details carry one fact per line", () => {
  assert.deepEqual(failureDetails(failure()), [
    "kind: server_error",
    "status: 503",
    "provider code: overloaded",
    "request id: req_1",
    "tried 3 times over 4.2 s",
    "the provider asked to wait 60 s",
    "cause:",
    "upstream 503",
  ])
})

test("the details leave out what the failure does not have", () => {
  assert.deepEqual(failureDetails(failure({ status: undefined, providerCode: undefined, requestId: undefined, attempts: 1, retryAfterMs: undefined, cause: [] })), [
    "kind: server_error",
  ])
})

test("the retry status counts down and stops at zero", () => {
  const retry = { attempt: 3, maxAttempts: 8, retryAtMs: 10_000, kind: "server_error", reason: "The server is overloaded." }
  assert.equal(retrySeconds(10_000, 6_000), 4)
  assert.equal(retrySeconds(10_000, 10_000), 0)
  assert.equal(retrySeconds(10_000, 12_000), 0)
  assert.equal(retryStatus(retry, retrySeconds(10_000, 6_000)), "Retrying 3/8 in 4 s · The provider reported a server error: The server is overloaded · Stop response")
  assert.equal(retryStatus(retry, retrySeconds(10_000, 10_000)), "Waiting for the provider · The provider reported a server error: The server is overloaded · Stop response")
})

test("the announcement follows the attempt, not the countdown", () => {
  const retry = { attempt: 3, maxAttempts: 8, retryAtMs: 10_000, kind: "server_error", reason: "The server is overloaded." }
  const first = retryAnnouncement(retry)
  assert.equal(retryAnnouncement({ ...retry, retryAtMs: 20_000 }), first)
  assert.notEqual(retryAnnouncement({ ...retry, attempt: 4 }), first)
  assert.equal(retryAnnouncement(undefined), "")
})
