import { computed, ref } from "vue"
import { defineStore } from "pinia"
import { readCreate, readSummaries } from "../rpc/read"
import type { ModelId, SessionSummary } from "../rpc/protocol"
import { useConnectionStore } from "./connection"

/** Sessions that share a working directory: the sidebar's workspace groups. */
export type Workspace = { cwd: string; name: string; sessions: SessionSummary[]; updatedAt: number }

/** The session directory (`session.summaries`) and session creation. */
export const useSessionsStore = defineStore("sessions", () => {
  const summaries = ref<SessionSummary[]>([])
  /** False until the first directory reply. */
  const loaded = ref(false)
  const error = ref("")
  let refreshing: Promise<void> | undefined

  const byId = computed(() => new Map(summaries.value.map((session) => [session.sessionId, session])))

  /** Workspaces by latest activity; sessions in each by latest activity. */
  const workspaces = computed<Workspace[]>(() => {
    const groups = new Map<string, SessionSummary[]>()
    for (const session of summaries.value) {
      const group = groups.get(session.cwd)
      if (group === undefined) groups.set(session.cwd, [session])
      else group.push(session)
    }
    return [...groups].map(([cwd, sessions]) => {
      sessions.sort((left, right) => right.updatedAt - left.updatedAt)
      return { cwd, name: workspaceName(cwd), sessions, updatedAt: sessions[0]!.updatedAt }
    }).sort((left, right) => right.updatedAt - left.updatedAt)
  })

  /** Reloads the directory; concurrent callers share one request. */
  function refresh(): Promise<void> {
    refreshing ??= useConnectionStore().client.call("session.summaries", {}, readSummaries)
      .then((list) => {
        summaries.value = list
        loaded.value = true
        error.value = ""
      })
      .catch((cause: unknown) => {
        error.value = cause instanceof Error ? cause.message : String(cause)
      })
      .finally(() => { refreshing = undefined })
    return refreshing
  }

  /** Creates a session in `cwd` (an absolute directory); `model` overrides the directory's default. */
  async function create(cwd: string, model: ModelId | undefined): Promise<string> {
    const params: Record<string, unknown> = { cwd }
    if (model !== undefined) params.model = { provider: model.providerId, modelId: model.modelId }
    const created = await useConnectionStore().client.call("session.create", params, readCreate)
    void refresh()
    return created.sessionId
  }

  /** Mirrors a live session's running state between directory refreshes. */
  function setRunning(sessionId: string, running: boolean): void {
    const session = byId.value.get(sessionId)
    if (session !== undefined && session.running !== running) session.running = running
  }

  function forget(sessionId: string): void {
    summaries.value = summaries.value.filter((session) => session.sessionId !== sessionId)
  }

  return { summaries, loaded, error, byId, workspaces, refresh, create, setRunning, forget }
})

export function workspaceName(cwd: string): string {
  const trimmed = cwd.replace(/[\\/]+$/, "")
  return trimmed.split(/[\\/]/).at(-1) || cwd
}
