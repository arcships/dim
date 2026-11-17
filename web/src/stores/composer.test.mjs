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
const { useComposerStore } = await server.ssrLoadModule("/src/stores/composer.ts")

function storage(initial) {
  const data = new Map()
  if (initial !== undefined) data.set("dim-web.send-mode.v1", initial)
  return {
    getItem: (key) => data.get(key) ?? null,
    setItem: (key, value) => data.set(key, value),
    value: () => data.get("dim-web.send-mode.v1"),
  }
}

beforeEach(() => setActivePinia(createPinia()))

test("a browser with no saved choice steers during a run", () => {
  globalThis.localStorage = storage()
  const composer = useComposerStore()
  assert.equal(composer.sendMode, "steer")
  assert.equal(composer.alternate(), "queue")
})

test("only a saved queue changes the default, and saving it survives a reload", async () => {
  globalThis.localStorage = storage(JSON.stringify({ sendMode: "queue" }))
  const composer = useComposerStore()
  assert.equal(composer.sendMode, "queue")
  assert.equal(composer.alternate(), "steer")

  composer.sendMode = "steer"
  await Promise.resolve()
  assert.deepEqual(JSON.parse(globalThis.localStorage.value()), { sendMode: "steer" })
  setActivePinia(createPinia())
  assert.equal(useComposerStore().sendMode, "steer", "the choice is read back from this browser")
})

test("a missing or unknown saved value means steer, and an unreadable one is not an error", () => {
  for (const stored of ['{"sendMode":"append"}', '{"sendMode":null}', "[]", "null", "{not json"]) {
    globalThis.localStorage = storage(stored)
    setActivePinia(createPinia())
    assert.equal(useComposerStore().sendMode, "steer", stored)
  }
})

test("a browser that refuses the write keeps the choice for this page and says it will not survive", async () => {
  globalThis.localStorage = storage()
  globalThis.localStorage.setItem = () => { throw new Error("quota") }
  const composer = useComposerStore()
  composer.sendMode = "queue"
  await Promise.resolve()
  assert.equal(composer.sendMode, "queue", "the choice still applies to this page")
  assert.match(composer.error, /will not survive a reload/)
})
