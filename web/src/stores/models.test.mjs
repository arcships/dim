import assert from "node:assert/strict"
import { fileURLToPath } from "node:url"
import { after, test } from "node:test"
import { createPinia, setActivePinia } from "pinia"
import { createServer } from "vite"

const server = await createServer({
  root: fileURLToPath(new URL("../../", import.meta.url)),
  configFile: false,
  server: { middlewareMode: true, ws: false },
  appType: "custom",
})
after(() => server.close())
const { useModels } = await server.ssrLoadModule("/src/stores/models.ts")
const { useConnectionStore } = await server.ssrLoadModule("/src/stores/connection.ts")

function discovery() {
  setActivePinia(createPinia())
  const connection = useConnectionStore()
  connection.hello = { instanceId: "test", serverVersion: "test", protocolVersion: 4, features: ["provider.models.query"] }
  const calls = []
  connection.client.call = async (method, params, read) => {
    calls.push({ method, params })
    assert.equal(method, "provider.models.query")
    const id = params.after ? "second" : "first"
    return read({ models: [{ providerId: "local", providerName: "Local", disabled: false, model: { id, name: id } }], next: params.after ? null : { providerId: "local", modelId: id } })
  }
  return { models: useModels(), calls, connection }
}

test("model views query one page and fetch the next only on demand", async () => {
  const { models, calls } = discovery()
  assert.equal(calls.length, 0)
  await models.load({ query: "reasoning" })
  assert.equal(calls.length, 1)
  assert.deepEqual(calls[0].params, { query: "reasoning" })
  assert.equal(models.providers[0].models.length, 1)
  await models.loadMore()
  assert.deepEqual(calls[1].params, { query: "reasoning", after: { providerId: "local", modelId: "first" } })
  assert.equal(models.providers[0].models.length, 2)
  assert.equal(models.next, undefined)
  models.clear()
  assert.equal(models.providers, undefined)
})

test("a second view owns no results from the first view", async () => {
  const { models, calls } = discovery()
  await models.load()
  const second = useModels()
  assert.equal(second.providers, undefined)
  await second.load({ providerId: "local", modelId: "first" })
  assert.equal(calls.length, 2)
  second.clear()
  assert.equal(models.providers[0].models.length, 1)
})

test("a closed view ignores an outstanding page", async () => {
  const { models, connection } = discovery()
  const pending = Promise.withResolvers()
  connection.client.call = async () => pending.promise
  const loaded = models.load()
  models.clear()
  pending.resolve({ models: [], next: undefined })
  await loaded
  assert.equal(models.providers, undefined)
})

test("refresh supersedes a page already in flight", async () => {
  const { models, connection } = discovery()
  const old = Promise.withResolvers()
  let refreshed = false
  connection.client.call = async (method, params, read) => {
    if (method === "provider.refresh") { refreshed = true; return read({}) }
    if (!refreshed) { await old.promise; return read({ models: [], next: null }) }
    return read({ models: [{ providerId: "managed", providerName: "Account", disabled: false, model: { id: "new", name: "New" } }], next: null })
  }
  const initial = models.load()
  await models.refresh("managed")
  old.resolve()
  await initial
  assert.equal(models.providers[0].models[0].id, "new")
})
