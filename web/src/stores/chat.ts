import { shallowReactive } from "vue"
import { defineStore } from "pinia"
import { Projection, type TimelineRow } from "../projection"
import { ConnectionLost, RpcError } from "../rpc/client"
import { MAIN_BRANCH, type ApprovalDecision, type ApprovalRequest, type ModelId, type Notification } from "../rpc/protocol"
import { ProtocolError, readAck, readHistory, readModelUpdate } from "../rpc/read"
import { useConnectionStore } from "./connection"
import { useSessionsStore } from "./sessions"

/** What the chat page shows of one attached session. */
export type SessionView = {
  sessionId: string
  /** `loading` until the first history load finishes; `closed` once the session is gone. */
  status: "loading" | "ready" | "closed" | "failed"
  error: string
  rows: readonly TimelineRow[]
  running: boolean
  stopping: boolean
  /** Pending approvals of every branch, oldest first. */
  approvals: ApprovalRequest[]
  model: ModelId | undefined
}

type Channel = {
  view: SessionView
  projection: Projection
  /** While attaching and paging history, live events wait here so they apply after it, in order. */
  syncing: boolean
  queued: Notification[]
  publishTimer: ReturnType<typeof setTimeout> | undefined
}

const HISTORY_PAGE = 256
const HISTORY_TIMEOUT_MS = 60_000
/** Snapshots go out one frame after the first change, however fast events arrive. */
const PUBLISH_INTERVAL_MS = 16

/**
 * Attached sessions: each one's projection, running state and approvals. A
 * session stays attached for the life of the tab once opened, and every
 * attached session re-attaches and catches up after a reconnect.
 */
export const useChatStore = defineStore("chat", () => {
  const views = shallowReactive<Record<string, SessionView>>({})
  const channels = new Map<string, Channel>()
  let refreshTimer: ReturnType<typeof setTimeout> | undefined

  function client() {
    return useConnectionStore().client
  }

  /** Attaches the session (once) and loads its history. */
  function open(sessionId: string): void {
    if (channels.has(sessionId)) return
    const view = shallowReactive<SessionView>({
      sessionId,
      status: "loading",
      error: "",
      rows: [],
      running: useSessionsStore().byId.get(sessionId)?.running ?? false,
      stopping: false,
      approvals: [],
      model: useSessionsStore().byId.get(sessionId)?.model,
    })
    const channel: Channel = { view, projection: new Projection(), syncing: false, queued: [], publishTimer: undefined }
    channels.set(sessionId, channel)
    views[sessionId] = view
    void sync(channel)
  }

  /** After a reconnect: attach again and fetch the history after the last applied event. */
  function resync(): void {
    for (const channel of channels.values()) {
      if (channel.view.status !== "closed") void sync(channel)
    }
  }

  async function sync(channel: Channel): Promise<void> {
    const { view, projection } = channel
    channel.syncing = true
    channel.queued = []
    // The server replays the approvals still pending when it attaches.
    view.approvals = []
    try {
      await client().call("session.attach", { sessionId: view.sessionId }, readAck)
      let after = projection.cursor
      let through: number | undefined
      for (;;) {
        const page = await client().call("session.history", {
          sessionId: view.sessionId,
          branchId: MAIN_BRANCH,
          after,
          limit: HISTORY_PAGE,
          ...(through === undefined ? {} : { through }),
        }, readHistory, HISTORY_TIMEOUT_MS)
        if ((through !== undefined && page.through !== through) || page.nextAfter < after || page.nextAfter > page.through || (!page.done && page.nextAfter === after)) {
          throw new ProtocolError("Invalid history page cursor")
        }
        through = page.through
        for (const event of page.events) projection.apply(event)
        after = page.nextAfter
        if (page.done) break
      }
      for (const notification of channel.queued) apply(channel, notification)
      view.status = "ready"
      view.error = ""
    } catch (error) {
      // A lost connection retries on reconnect; anything else is shown.
      if (error instanceof RpcError && error.appError === "session_closed") view.status = "closed"
      else if (!(error instanceof ConnectionLost)) {
        view.status = "failed"
        view.error = error instanceof Error ? error.message : String(error)
      }
    } finally {
      channel.syncing = false
      channel.queued = []
      publish(channel)
    }
  }

  function receive(notification: Notification): void {
    const sessionId = notification.method === "session.model.progress"
      ? notification.progress.sessionId
      : notification.method === "approval.request" ? notification.request.sessionId : notification.sessionId
    const channel = channels.get(sessionId)
    if (channel === undefined) return
    if (channel.syncing && (notification.method === "session.event" || notification.method === "session.model.progress")) {
      channel.queued.push(notification)
      return
    }
    apply(channel, notification)
  }

  function apply(channel: Channel, notification: Notification): void {
    const view = channel.view
    switch (notification.method) {
      case "session.event":
        if (notification.branchId === MAIN_BRANCH && channel.projection.apply(notification.event)) changed(channel)
        break
      case "session.model.progress":
        if (notification.progress.branchId === MAIN_BRANCH) {
          channel.projection.progress(notification.progress)
          changed(channel)
        }
        break
      case "session.state":
        if (notification.branchId !== MAIN_BRANCH) break
        if (view.running && !notification.running) refreshDirectorySoon()
        view.running = notification.running
        view.stopping = notification.stopping
        useSessionsStore().setRunning(view.sessionId, notification.running)
        break
      case "approval.request":
        // A replayed request may repeat one already shown.
        if (!view.approvals.some((approval) => approval.requestId === notification.request.requestId)) {
          view.approvals = [...view.approvals, notification.request]
        }
        break
      case "approval.cancelled":
        removeApproval(view, notification.requestId)
        break
      case "session.closed":
        view.status = "closed"
        view.running = false
        view.approvals = []
        useSessionsStore().forget(view.sessionId)
        break
    }
  }

  function changed(channel: Channel): void {
    channel.publishTimer ??= setTimeout(() => publish(channel), PUBLISH_INTERVAL_MS)
  }

  function publish(channel: Channel): void {
    clearTimeout(channel.publishTimer)
    channel.publishTimer = undefined
    channel.view.rows = channel.projection.rows.slice()
    channel.view.model = channel.projection.model ?? useSessionsStore().byId.get(channel.view.sessionId)?.model
  }

  /** A finished turn changes the directory's preview and activity time. */
  function refreshDirectorySoon(): void {
    clearTimeout(refreshTimer)
    refreshTimer = setTimeout(() => void useSessionsStore().refresh(), 500)
  }

  function removeApproval(view: SessionView, requestId: string): void {
    view.approvals = view.approvals.filter((approval) => approval.requestId !== requestId)
  }

  async function submit(sessionId: string, text: string): Promise<void> {
    await client().call("session.submit", {
      sessionId,
      branchId: MAIN_BRANCH,
      submissionId: crypto.randomUUID(),
      text,
      mode: "queue",
    }, readAck)
  }

  async function stop(sessionId: string): Promise<void> {
    await client().call("session.stop", { sessionId, branchId: MAIN_BRANCH, reason: "Stopped from the web workspace" }, readAck)
  }

  async function respond(sessionId: string, approval: ApprovalRequest, decision: ApprovalDecision): Promise<void> {
    await client().call("approval.respond", {
      sessionId,
      branchId: approval.branchId,
      requestId: approval.requestId,
      decision,
    }, readAck)
    const view = views[sessionId]
    if (view !== undefined) removeApproval(view, approval.requestId)
  }

  async function setModel(sessionId: string, model: ModelId): Promise<void> {
    const result = await client().call("session.model.update", {
      sessionId,
      branchId: MAIN_BRANCH,
      providerId: model.providerId,
      modelId: model.modelId,
    }, readModelUpdate)
    const channel = channels.get(sessionId)
    if (channel === undefined) return
    channel.projection.model = result.model
    publish(channel)
  }

  return { views, open, resync, receive, submit, stop, respond, setModel }
})
