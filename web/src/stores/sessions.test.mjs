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
const { useSessionsStore } = await server.ssrLoadModule("/src/stores/sessions.ts")
const { useConnectionStore } = await server.ssrLoadModule("/src/stores/connection.ts")

test("the sidebar omits internal sessions while known children retain their identity and parent", async () => {
  setActivePinia(createPinia())
  const base = {
    cwd: "/work",
    title: null,
    titleSource: null,
    titleRevision: 0,
    model: { providerId: "test", modelId: "model" },
    createdAt: 1,
    updatedAt: 1,
    running: false,
    sequence: 0,
  }
  useConnectionStore().client.call = async (method, params, read) => {
    assert.equal(method, "session.summaries")
    assert.deepEqual(params, {})
    return read([
      { ...base, sessionId: "parent", origin: "unknown" },
      { ...base, sessionId: "child", origin: "internal", parentSessionId: "parent" },
      { ...base, sessionId: "orphan", cwd: "/other", origin: "internal" },
      { ...base, sessionId: "acp", origin: "acp" },
      { ...base, sessionId: "older-daemon" },
    ])
  }
  const sessions = useSessionsStore()
  await sessions.refresh()
  assert.deepEqual(sessions.workspaces.map((workspace) => [workspace.cwd, workspace.sessions.map((session) => session.sessionId)]), [
    ["/work", ["parent", "acp", "older-daemon"]],
  ])
  assert.equal(sessions.byId.get("child").parentSessionId, "parent")
  assert.ok(sessions.byId.has("orphan"))
})
