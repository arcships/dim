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
const { useModelsStore } = await server.ssrLoadModule("/src/stores/models.ts")
const { useConnectionStore } = await server.ssrLoadModule("/src/stores/connection.ts")

function discovery() {
  setActivePinia(createPinia())
  const connection = useConnectionStore()
  connection.hello = { instanceId: "test", serverVersion: "test", protocolVersion: 4, features: ["provider.list", "provider.catalog", "account.providers"] }
  const state = { failedMethod: undefined, collision: false }
  connection.client.call = async (method, params, read) => {
    if (method === state.failedMethod) throw new Error(`${method} is reloading`)
    if (method === "provider.list") return read({ providers: [
      { providerId: "local", displayName: "Local", enabled: true, credentialConfigured: false },
      { providerId: "disabled", displayName: "Disabled", enabled: false, credentialConfigured: true },
      ...(state.collision ? [{ providerId: "managed", displayName: "Legacy static name", enabled: true, credentialConfigured: true }] : []),
    ] })
    if (method === "account.providers") return read([{ providerId: "managed", displayName: "Managed account" }])
    if (method === "provider.catalog") return read({ catalog: {
      models: [{ id: `${params.providerId}-model`, name: `${params.providerId} model` }],
      disabledModelIds: [],
    } })
    throw new Error(`Unexpected method ${method}`)
  }
  return { models: useModelsStore(), state, connection }
}

test("refresh updates the picker even if a catalog read was already in flight", async () => {
  const { models, connection } = discovery()
  const call = connection.client.call.bind(connection.client)
  const oldRead = Promise.withResolvers()
  const oldStarted = Promise.withResolvers()
  const remoteRefreshed = Promise.withResolvers()
  let refreshed = false
  connection.client.call = async (method, params, read) => {
    if (method === "provider.refresh") {
      assert.deepEqual(params, { providerId: "managed" })
      refreshed = true
      remoteRefreshed.resolve()
      return read({})
    }
    if (method === "provider.catalog" && params.providerId === "managed") {
      if (!refreshed) {
        oldStarted.resolve()
        await oldRead.promise
        return read({ catalog: { models: [], disabledModelIds: [] } })
      }
      return read({ catalog: { models: [{ id: "new", name: "New model" }], disabledModelIds: [] } })
    }
    return call(method, params, read)
  }
  const initialLoad = models.load()
  await oldStarted.promise
  const refresh = models.refresh("managed")
  await remoteRefreshed.promise
  oldRead.resolve()
  await Promise.all([initialLoad, refresh])
  assert.deepEqual(models.providers.find((provider) => provider.providerId === "managed")?.models, [
    { id: "new", name: "New model" },
  ])
})

test("model discovery merges provider identities with the managed descriptor taking precedence", async () => {
  const { models, state } = discovery()
  state.collision = true
  await models.load()
  assert.deepEqual(models.providers, [
    { providerId: "local", name: "Local", models: [{ id: "local-model", name: "local model" }] },
    { providerId: "managed", name: "Managed account", models: [{ id: "managed-model", name: "managed model" }] },
  ])
  assert.equal(models.error, "")
})

for (const [failedMethod, expected] of [
  ["account.providers", [{ providerId: "local", name: "Local", models: [{ id: "local-model", name: "local model" }] }]],
  ["provider.list", [{ providerId: "managed", name: "Managed account", models: [{ id: "managed-model", name: "managed model" }] }]],
]) {
  test(`model discovery retains healthy models when ${failedMethod} fails and recovers on retry`, async () => {
    const { models, state } = discovery()
    state.failedMethod = failedMethod
    await models.load()
    assert.deepEqual(models.providers, expected)
    assert.equal(models.error, `${failedMethod} is reloading`)

    state.failedMethod = undefined
    await models.load()
    assert.deepEqual(models.providers, [
      { providerId: "local", name: "Local", models: [{ id: "local-model", name: "local model" }] },
      { providerId: "managed", name: "Managed account", models: [{ id: "managed-model", name: "managed model" }] },
    ])
    assert.equal(models.error, "")
  })
}
