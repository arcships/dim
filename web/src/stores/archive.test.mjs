import assert from "node:assert/strict"
import { fileURLToPath } from "node:url"
import { after, beforeEach, test } from "node:test"
import { createPinia, setActivePinia } from "pinia"
import { createServer } from "vite"

const server = await createServer({
  root: fileURLToPath(new URL("../../", import.meta.url)),
  configFile: false,
  server: { middlewareMode: true, ws: false },
  appType: "custom",
})
after(() => server.close())
const { useArchiveStore } = await server.ssrLoadModule("/src/stores/archive.ts")

function storage(initial) {
  const data = new Map()
  if (initial !== undefined) data.set("dim-web.archive.v1", initial)
  return {
    getItem: key => data.get(key) ?? null,
    setItem: (key, value) => data.set(key, value),
    value: () => data.get("dim-web.archive.v1"),
  }
}

beforeEach(() => setActivePinia(createPinia()))

test("reads only the string ids of the stored array", () => {
  globalThis.localStorage = storage(JSON.stringify(["alpha", 7, null, "beta"]))
  const archive = useArchiveStore()
  assert.deepEqual(archive.hidden, ["alpha", "beta"])
})

test("a malformed stored value contributes nothing and the first archive writes the array", () => {
  globalThis.localStorage = storage("{not json")
  const archive = useArchiveStore()
  assert.deepEqual(archive.hidden, [])
  assert.equal(archive.archive("alpha"), true)
  assert.deepEqual(JSON.parse(globalThis.localStorage.value()), ["alpha"])
  assert.equal(archive.has("alpha"), true)
})

test("restore and drop rewrite the key, and no Session is deleted by either", () => {
  globalThis.localStorage = storage(JSON.stringify(["alpha", "beta", "gone"]))
  const archive = useArchiveStore()
  assert.equal(archive.restore("alpha"), true)
  assert.equal(archive.drop(["gone"]), true)
  assert.deepEqual(archive.hidden, ["beta"])
  assert.deepEqual(JSON.parse(globalThis.localStorage.value()), ["beta"])
})

test("a failed write keeps the row visible", () => {
  globalThis.localStorage = storage(JSON.stringify(["alpha"]))
  const archive = useArchiveStore()
  globalThis.localStorage.setItem = () => { throw new Error("quota") }
  assert.equal(archive.archive("beta"), false)
  assert.deepEqual(archive.hidden, ["alpha"])
})

test("an unreadable key fails changes instead of clobbering markers", () => {
  globalThis.localStorage = storage(JSON.stringify(["alpha"]))
  const archive = useArchiveStore()
  globalThis.localStorage.getItem = () => { throw new Error("denied") }
  assert.equal(archive.archive("beta"), false)
  assert.equal(archive.restore("alpha"), false)
  assert.equal(archive.drop(["alpha"]), false)
  assert.deepEqual(archive.hidden, ["alpha"])
})

test("sequential changes from two tabs merge with the key's current value", () => {
  globalThis.localStorage = storage(JSON.stringify(["alpha"]))
  setActivePinia(createPinia())
  const firstTab = useArchiveStore()
  setActivePinia(createPinia())
  const secondTab = useArchiveStore()

  assert.equal(firstTab.archive("beta"), true)
  assert.deepEqual(JSON.parse(globalThis.localStorage.value()), ["alpha", "beta"])

  assert.equal(secondTab.restore("beta"), true, "restore sees the id the other tab added")
  assert.deepEqual(JSON.parse(globalThis.localStorage.value()), ["alpha"])
  assert.deepEqual(secondTab.hidden, ["alpha"])

  assert.equal(secondTab.archive("gamma"), true)
  assert.deepEqual(JSON.parse(globalThis.localStorage.value()), ["alpha", "gamma"])

  assert.equal(firstTab.drop(["gamma"]), true, "cleanup keeps changes another tab made")
  assert.deepEqual(JSON.parse(globalThis.localStorage.value()), ["alpha"])
})

test("a storage event for this key refreshes the list", t => {
  globalThis.localStorage = storage(JSON.stringify(["alpha"]))
  const listeners = []
  globalThis.window = { addEventListener: (type, handler) => { if (type === "storage") listeners.push(handler) } }
  t.after(() => { delete globalThis.window })
  const archive = useArchiveStore()
  assert.equal(listeners.length, 1)

  globalThis.localStorage.setItem("dim-web.archive.v1", JSON.stringify(["alpha", "beta"]))
  listeners[0]({ key: "dim-web.archive.v1" })
  assert.deepEqual(archive.hidden, ["alpha", "beta"])

  listeners[0]({ key: "another-key" })
  assert.deepEqual(archive.hidden, ["alpha", "beta"], "another key does not touch this list")

  globalThis.localStorage.setItem("dim-web.archive.v1", JSON.stringify([]))
  listeners[0]({ key: null })
  assert.deepEqual(archive.hidden, [], "clearing storage clears the list")
})
