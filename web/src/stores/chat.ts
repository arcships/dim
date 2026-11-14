import { shallowReactive } from "vue"
import { defineStore } from "pinia"
import { alwaysCommand, alwaysRule, fileGrant, sameRule } from "../approvals"
import { Projection, type TimelineRow } from "../projection"
import { HistoryWindow } from "../history-window"
import { Lines, type LineView } from "../lines"
import { ConnectionLost, RpcError } from "../rpc/client"
import type { Goal, GoalCall } from "../goal"
import { readLoops as readLoopsReply, readStartedLoop, type Loop } from "../loop"
import { readUploadedImage } from "../rpc/read"
import { MAIN_BRANCH, type ApprovalDecision, type ApprovalRequest, type DraftAttachments, type FileCandidate, type ModelId, type ModelProgress, type Notification, type PendingQuestion, type PermissionPreset, type ModelRetryProgress, type PermissionPolicy, type QueuedInput, type QuestionAnswer, type SessionEvent, type UserInput, type WithdrawResult } from "../rpc/protocol"
import { readRevision, readRules } from "../rpc/general-settings"
import { ProtocolError, readAttached, readBranches, readBranchDetail, readPendingInputs, readPendingQuestions, readTurn, readGoal, readTool, readAck, readAttachmentPath, readBranchCreate, readCompactResult, readConfigurationOutcome, readContextUsage, readFileCandidates, readForkedSessionId, readHistory, readDisplayHistory, readModelUpdate, readMemberApproval, readPermissionState, readPermissionVersion, readSessionMethods, readWithdrawResult } from "../rpc/read"
import { useConnectionStore } from "./connection"
import { useSessionsStore } from "./sessions"
import { useUiStore } from "./ui"

/** What a permission change did: written, already in place, or refused because `main` changed first. */
export type PermissionOutcome = "updated" | "unchanged" | "conflict"

/** One line's transcript and history state. */
export type BranchView = {
  rows: readonly TimelineRow[]
  running: boolean
  stopping: boolean
  status: "loading" | "ready" | "failed"
  error: string
  lastTurnEnd: number | undefined
  earlier: boolean
  loadingEarlier: boolean
  historyError: string
}

/** What the chat page shows of one attached session. */
export type SessionView = {
  sessionId: string
  /** `loading` until the first main-history load finishes; `closed` once the session is gone. */
  status: "loading" | "ready" | "closed" | "failed"
  error: string
  /** The main line's transcript. */
  rows: readonly TimelineRow[]
  /** Whether main runs, and whether it is stopping. */
  running: boolean
  stopping: boolean
  /** The retry main's current model request is waiting to make, while it sleeps. */
  retry: ModelRetryProgress | undefined
  /** Whether any line of this session runs: the header and sidebar badge. */
  sessionRunning: boolean
  /** Pending approvals of every branch, oldest first. */
  approvals: ApprovalRequest[]
  /** Waiting `ask_user` questions of every branch, oldest first. */
  questions: PendingQuestion[]
  /** The method names this Session serves (`session.methods`); empty when the daemon offers no table. */
  methods: readonly string[]
  /** The policy of `main`: read after history loads, then kept current by `permission/changed`. */
  permission: PermissionPolicy | undefined
  /** The main line's current goal from its owner. */
  goal: Goal | undefined
  /** Messages queued on the main line while it runs, oldest first. */
  queued: readonly QueuedInput[]
  model: ModelId | undefined
  /** Every line of the session, main first, in tree order. */
  lines: LineView[]
  /** Opened line transcripts, by branch id. */
  branches: Record<string, BranchView>
  lastTurnEnd: number | undefined
  earlier: boolean
  loadingEarlier: boolean
  historyError: string
}

type BranchStream = {
  projection: Projection
  liveTask: Promise<void> | undefined
  progress: ModelProgress | undefined
  syncing: boolean
  done: boolean
  error: string
  opened: boolean
  before: number | undefined
  through: number
  loadingEarlier: boolean
  historyError: string
  /** Live events held while this line's history is paged in. */
  queued: SessionEvent[]
}

type Channel = {
  view: SessionView
  main: BranchStream
  branches: Map<string, BranchStream>
  lines: Lines
  states: Map<string, { running: boolean; stopping: boolean }>
  /** While `syncing`, every notification waits here so history applies first. */
  syncing: boolean
  queued: Notification[]
  historyQueue: string[]
  historyRunning: boolean
  goalRefreshing: boolean
  goalDirty: boolean
  publishTimer: ReturnType<typeof setTimeout> | undefined
}

const HISTORY_TIMEOUT_MS = 60_000
/** Snapshots go out one frame after the first change, however fast events arrive. */
const PUBLISH_INTERVAL_MS = 16

function stream(): BranchStream {
  return { projection: new Projection(), opened: false, before: undefined, through: 0, loadingEarlier: false, historyError: "", liveTask: undefined, progress: undefined, syncing: false, done: false, error: "", queued: [] }
}

function idleState(): { running: boolean; stopping: boolean } {
  return { running: false, stopping: false }
}

/**
 * Attached sessions: each line's projection, running state and approvals. A
 * session stays attached for the life of the tab once opened, and every
 * attached session re-attaches and replaces its opened windows after reconnect.
 */
export const useChatStore = defineStore("chat", () => {
  const views = shallowReactive<Record<string, SessionView>>({})
  const channels = new Map<string, Channel>()
  let refreshTimer: ReturnType<typeof setTimeout> | undefined

  function client() {
    return useConnectionStore().client
  }

  /** Attaches the session (once) and loads its main history. */
  function open(sessionId: string): void {
    if (channels.has(sessionId)) return
    const stored = useSessionsStore().byId.get(sessionId)
    const view = shallowReactive<SessionView>({
      sessionId,
      status: "loading",
      error: "",
      rows: [],
      running: stored?.running ?? false,
      stopping: false,
      retry: undefined,
      sessionRunning: stored?.running ?? false,
      approvals: [],
      questions: [],
      methods: [],
      permission: undefined,
      goal: undefined,
      queued: [],
      model: stored?.model,
      lines: [],
      branches: {},
      lastTurnEnd: undefined,
      earlier: false, loadingEarlier: false, historyError: "",
    })
    const channel: Channel = {
      view,
      main: stream(),
      branches: new Map(),
      lines: new Lines(),
      states: new Map(),
      syncing: false,
      queued: [],
      historyQueue: [],
      historyRunning: false,
      goalRefreshing: false, goalDirty: false,
      publishTimer: undefined,
    }
    channel.main.opened = true
    channels.set(sessionId, channel)
    views[sessionId] = view
    void sync(channel)
  }

  /** Reattach and replace opened transcripts with bounded windows. */
  async function resync(): Promise<void> {
    await Promise.all([...channels.values()].filter(channel => channel.view.status !== "closed").map(sync))
  }

  async function sync(channel: Channel): Promise<void> {
    if (channel.syncing) return
    const { view } = channel
    channel.syncing = true
    channel.view.status = "loading"
    channel.states.clear()
    channel.queued = []
    view.approvals = []
    channel.main.syncing = true
    channel.main.done = false
    channel.main.error = ""
    channel.main.queued = []
    channel.main.progress = undefined
    for (const [id, branch] of channel.branches) {
      branch.syncing = branch.opened
      branch.done = false
      branch.error = ""
      branch.queued = []
      branch.progress = undefined
      if (branch.opened && !channel.historyQueue.includes(id)) channel.historyQueue.push(id)
    }
    try {
      const model = await client().call("session.attach", { sessionId: view.sessionId, ...((useConnectionStore().supports("session.history.display") && useConnectionStore().supports("session.history.detail")) ? { display: true } : {}) }, readAttached)
      if (!useConnectionStore().supports("session.history.latestTurn")) throw new ProtocolError("This server does not support bounded conversation history")
      view.methods = await sessionMethods(view.sessionId)
      for (const required of ["session.input.pending", "session.question.pending", "session.turn.get"]) {
        if (!view.methods.includes(required)) throw new ProtocolError(`This Session does not provide ${required}`)
      }
      const directory = await client().call("session.branches", { sessionId: view.sessionId }, readBranches)
      channel.lines.directory(directory)
      for (const entry of directory) {
        if (entry.branchId === MAIN_BRANCH) continue
        const branch = ensureBranch(channel, entry.branchId)
        if (!branch.opened) {
          const asks = await pendingQuestions(channel, entry.branchId, entry.head)
          branch.projection = new Projection()
          branch.projection.restoreControls([], asks, entry.head)
        }
      }
      const main = await loadHistory(channel, MAIN_BRANCH)
      if (main !== "ok") {
        if (main === "failed" && view.status !== "closed") {
          view.status = "failed"
          view.error = channel.main.error
        }
        return
      }
      channel.main.projection.model = model
      await refreshGoal(channel)
      if (view.status === "failed") return
      const queued = channel.queued
      channel.queued = []
      channel.syncing = false
      for (const notification of queued) receive(notification)
      if (view.status !== "closed") view.permission = await sessionPermission(view.sessionId)
      if (view.status !== "closed") {
        view.status = "ready"
        view.error = ""
      }
      publish(channel)
      if (view.status !== "closed") void pumpHistory(channel)
    } catch (error) {
      if (error instanceof RpcError && error.appError === "session_closed") view.status = "closed"
      else if (!(error instanceof ConnectionLost)) {
        view.status = "failed"
        view.error = error instanceof Error ? error.message : String(error)
      }
    } finally {
      channel.syncing = false
      channel.main.syncing = !channel.main.done || channel.main.error !== ""
      if (channel.main.syncing) {
        for (const notification of channel.queued) applyNotification(channel, notification)
      }
      channel.queued = []
      publish(channel)
    }
  }

  function receive(notification: Notification): void {
    if (notification.method === "ui.extensions.changed") return
    const sessionId = notification.method === "session.model.progress"
      ? notification.progress.sessionId
      : notification.method === "approval.request" ? notification.request.sessionId : notification.sessionId
    const channel = channels.get(sessionId)
    if (channel === undefined) return
    if (channel.syncing) {
      channel.queued.push(notification)
      return
    }
    applyNotification(channel, notification)
  }

  function applyNotification(channel: Channel, notification: Notification): void {
    const view = channel.view
    switch (notification.method) {
      case "session.event":
      case "session.display": {
        const branchId = notification.branchId
        const stream = ensureBranch(channel, branchId)
        if (stream.syncing) {
          channel.lines.apply(branchId, notification.event)
          const content = notification.event.content
          if (content.kind === "turn-end" || ((content.kind === "model-end" || content.kind === "tool-use-start" || content.kind === "tool-use") && content.requestId === stream.progress?.requestId)) stream.progress = undefined
          stream.queued.push(notification.event)
        } else if (!stream.opened) {
          applyEvent(channel, branchId, notification.event)
        } else {
          stream.queued.push(notification.event)
          void consumeLive(channel, branchId).catch(error => {
            stream.error = error instanceof Error ? error.message : String(error)
            stream.syncing = true
            changed(channel)
          })
        }
        break
      }
      case "session.model.progress": {
        const branchId = notification.progress.branchId
        const stream = ensureBranch(channel, branchId)
        stream.progress = notification.progress
        if (stream.syncing || !stream.opened || stream.liveTask !== undefined) break
        stream.projection.progress(notification.progress)
        changed(channel)
        break
      }
      case "session.state": {
        if (notification.branchId === MAIN_BRANCH) {
          if (view.running && !notification.running) refreshDirectorySoon()
        }
        channel.states.set(notification.branchId, { running: notification.running, stopping: notification.stopping })
        channel.lines.setRunning(notification.branchId, notification.running)
        if (notification.branchId !== MAIN_BRANCH) ensureBranch(channel, notification.branchId)
        const running = anyRunning(channel)
        view.running = stateOf(channel, MAIN_BRANCH).running
        view.stopping = stateOf(channel, MAIN_BRANCH).stopping
        view.sessionRunning = running
        useSessionsStore().setRunning(view.sessionId, running)
        changed(channel)
        break
      }
      case "approval.request":
        if (!view.approvals.some((approval) => approval.requestId === notification.request.requestId && approval.branchId === notification.request.branchId)) {
          view.approvals = [...view.approvals, notification.request]
        }
        break
      case "approval.resolved":
      case "approval.cancelled":
        if (view.approvals.some((approval) => approval.branchId === notification.branchId && approval.requestId === notification.requestId)) removeApproval(view, notification.branchId, notification.requestId)
        break
      case "memberApprovalEnded": {
        const ended = view.approvals.filter((approval) => approval.branchId === notification.branchId && approval.call.name === "member_approval_allow" && approval.call.input.approvalId === notification.approvalId)
        for (const approval of ended) {
          removeApproval(view, approval.branchId, approval.requestId)
          void respond(view.sessionId, approval, "deny").catch(() => {})
        }
        break
      }
      case "session.closed":
        view.status = "closed"
        view.running = false
        view.stopping = false
        view.sessionRunning = false
        view.approvals = []
        view.questions = []
        useSessionsStore().forget(view.sessionId)
        useUiStore().resetSessionTabs(view.sessionId)
        break
    }
  }

  function applyEvent(channel: Channel, branchId: string, event: SessionEvent): void {
    channel.lines.apply(branchId, event)
    const stream = ensureBranch(channel, branchId)
    if (!stream.opened) {
      stream.projection.applyControl(event)
      changed(channel)
      return
    }
    if (!stream.projection.apply(event)) return
    const applied = event.content
    if (applied.kind === "turn-end" || ((applied.kind === "model-end" || applied.kind === "tool-use-start" || applied.kind === "tool-use") && applied.requestId === stream.progress?.requestId)) stream.progress = undefined
    if (event.content.kind === "branch-create") ensureBranch(channel, event.content.branchId)
    if (branchId === MAIN_BRANCH && event.content.kind === "permission-changed") channel.view.permission = event.content.policy
    if (branchId === MAIN_BRANCH && (applied.kind === "input-deleted" || ((applied.kind === "input" || applied.kind === "source-input") && applied.blocks.some(block => block.key?.startsWith("goal:"))))) void refreshGoal(channel)
    changed(channel)
  }

  async function hydrateTools(channel: Channel, branchId: string, projection: Projection, event: SessionEvent): Promise<void> {
    if (event.parentCallId !== undefined) return
    const content = event.content
    if (content.kind !== "tool-completed" && content.kind !== "tool-result") return
    const results = content.kind === "tool-result" ? content.results : [{ callId: content.callId, result: content.result }]
    for (const result of results) {
      if (projection.rows.some(row => row.kind === "tool" && row.callId === result.callId)) continue
      const call = await client().call("session.tool.get", { sessionId: channel.view.sessionId, branchId, requestId: content.requestId, callId: result.callId, through: event.sequence }, readTool)
      if (call !== undefined) projection.rows.push({ kind: "tool", key: `tool-${call.callId}`, callId: call.callId, name: call.name, input: call.input, pending: false, result: result.result })
    }
  }

  function consumeLive(channel: Channel, branchId: string): Promise<void> {
    const branch = ensureBranch(channel, branchId)
    if (branch.liveTask !== undefined) return branch.liveTask
    const projection = branch.projection
    branch.liveTask = (async () => {
      while (!branch.syncing && branch.queued.length > 0) {
        const event = branch.queued[0]!
        if (event.sequence > projection.cursor) await hydrateTools(channel, branchId, projection, event)
        if (branch.projection !== projection || branch.syncing) return
        branch.queued.shift()
        applyEvent(channel, branchId, event)
      }
      if (!branch.syncing && branch.progress !== undefined) projection.progress(branch.progress)
    })().then(() => {
      branch.liveTask = undefined
      if (!branch.syncing && branch.queued.length > 0) return consumeLive(channel, branchId)
    }, error => {
      branch.liveTask = undefined
      branch.syncing = true
      throw error
    }).finally(() => changed(channel))
    return branch.liveTask
  }

  function ensureBranch(channel: Channel, branchId: string): BranchStream {
    if (branchId === MAIN_BRANCH) return channel.main
    let branch = channel.branches.get(branchId)
    if (branch === undefined) {
      branch = stream()
      channel.branches.set(branchId, branch)
      changed(channel)
    }
    return branch
  }

  async function pendingQuestions(channel: Channel, branchId: string, through: number) {
    const asks: ReturnType<Projection["waiting"]>[number][] = []
    let after: unknown
    do {
      const page = await client().call("session.question.pending", { sessionId: channel.view.sessionId, branchId, through, ...(after == null ? {} : { after }) }, readPendingQuestions)
      if (page.branchId !== branchId || page.through !== through) throw new ProtocolError("Question snapshot changed")
      asks.push(...page.asks)
      after = page.next
    } while (after != null)
    return asks
  }

  async function restoreControls(channel: Channel, branchId: string, projection: Projection, through: number): Promise<void> {
    const asks = await pendingQuestions(channel, branchId, through)
    const inputs: QueuedInput[] = []
    let after: unknown
    do {
      const page = await client().call("session.input.pending", { sessionId: channel.view.sessionId, branchId, through, ...(after == null ? {} : { after }) }, readPendingInputs)
      if (page.branchId !== branchId || page.through !== through) throw new ProtocolError("Input snapshot changed")
      inputs.push(...page.inputs)
      after = page.next
    } while (after != null)
    projection.restoreControls(inputs, asks, through)
    const turn = await client().call("session.turn.get", { sessionId: channel.view.sessionId, branchId, through }, readTurn)
    if (turn.branchId !== branchId || turn.through !== through) throw new ProtocolError("Turn snapshot changed")
    if (turn.output !== undefined) projection.activeOutput(turn.output)
    if (turn.turn?.ended) projection.lastTurnEnd = Math.max(projection.lastTurnEnd ?? 0, turn.turn.sequence)
    if (!channel.states.has(branchId)) channel.states.set(branchId, { running: turn.turn !== undefined && !turn.turn.ended, stopping: false })
  }

  async function refreshGoal(channel: Channel): Promise<void> {
    if (!channel.view.methods.includes("session.goal.get")) return
    channel.goalDirty = true
    if (channel.goalRefreshing) return
    channel.goalRefreshing = true
    try {
      while (channel.goalDirty) {
        channel.goalDirty = false
        channel.main.projection.goal = await client().call("session.goal.get", { sessionId: channel.view.sessionId, branchId: MAIN_BRANCH }, readGoal)
        changed(channel)
      }
    } catch (error) {
      channel.view.status = "failed"
      channel.view.error = error instanceof Error ? error.message : String(error)
    } finally {
      channel.goalRefreshing = false
    }
  }

  async function window(channel: Channel, branchId: string, before?: number, through?: number) {
    const connection = useConnectionStore()
    const display = connection.supports("session.history.display") && connection.supports("session.history.detail")
    const window = new HistoryWindow(before, through, display && connection.supports("session.history.display.tail"), display && connection.supports("session.history.display.transcript"))
    for (;;) {
      const page = await client().call(display ? "session.history.display" : "session.history", window.params(channel.view.sessionId, branchId), display ? readDisplayHistory : readHistory, HISTORY_TIMEOUT_MS)
      if (window.push(page)) break
    }
    const events = window.take()
    const projection = new Projection()
    for (const event of events) {
      await hydrateTools(channel, branchId, projection, event)
      projection.apply(event)
    }
    return { window, events, projection }
  }

  async function loadHistory(channel: Channel, branchId: string): Promise<"ok" | "lost" | "failed"> {
    const branch = ensureBranch(channel, branchId)
    branch.syncing = true
    branch.error = ""
    branch.done = false
    for (let attempt = 0; ; attempt++) {
      try {
        const loaded = await window(channel, branchId)
        const through = loaded.window.through!
        await restoreControls(channel, branchId, loaded.projection, through)
        if (branchId !== MAIN_BRANCH) channel.lines.detail(await client().call("session.branch.get", { sessionId: channel.view.sessionId, branchId }, readBranchDetail))
        await branch.liveTask?.catch(() => {})
        loaded.projection.cursor = through
        branch.projection = loaded.projection
        branch.before = loaded.window.before
        branch.through = through
        for (const event of loaded.events) {
          const content = event.content
          if (content.kind === "turn-end" || ((content.kind === "model-end" || content.kind === "tool-use-start" || content.kind === "tool-use") && content.requestId === branch.progress?.requestId)) branch.progress = undefined
        }
        branch.queued = branch.queued.filter(event => event.sequence > through)
        branch.syncing = false
        await consumeLive(channel, branchId)
        branch.done = true
        changed(channel)
        return "ok"
      } catch (error) {
        if (branchId !== MAIN_BRANCH && attempt < 10 && transientBranch(error)) {
          await new Promise(resolve => setTimeout(resolve, 100))
          continue
        }
        if (error instanceof ConnectionLost) return "lost"
        branch.done = true
        branch.syncing = true
        branch.error = error instanceof Error ? error.message : String(error)
        if (error instanceof RpcError && error.appError === "session_closed") channel.view.status = "closed"
        changed(channel)
        return "failed"
      }
    }
  }

  async function loadEarlier(sessionId: string, branchId: string): Promise<void> {
    const channel = channels.get(sessionId)
    if (channel === undefined) return
    const branch = ensureBranch(channel, branchId)
    if (branch.before === undefined || branch.loadingEarlier || branch.syncing) return
    branch.loadingEarlier = true
    branch.historyError = ""
    const before = branch.before
    const projection = branch.projection
    publish(channel)
    try {
      const loaded = await window(channel, branchId, before, branch.through)
      if (branch.projection !== projection) return
      projection.prepend(loaded.projection.rows, `before-${before}`)
      branch.before = loaded.window.before
    } catch (error) {
      branch.historyError = error instanceof Error ? error.message : String(error)
    } finally {
      branch.loadingEarlier = false
      publish(channel)
    }
  }

  /** A line is published just after `branch/create`; its first history read can lose the race. */
  function transientBranch(error: unknown): boolean {
    return error instanceof RpcError
      && (error.appError === "branch_missing" || error.appError === "unknown_branch" || error.message.startsWith("unknown branch"))
  }

  async function pumpHistory(channel: Channel): Promise<void> {
    if (channel.historyRunning || channel.syncing) return
    channel.historyRunning = true
    try {
      while (channel.historyQueue.length > 0) {
        const branchId = channel.historyQueue.shift()!
        if (branchId === MAIN_BRANCH || !channel.branches.has(branchId)) continue
        await loadHistory(channel, branchId)
      }
    } finally {
      channel.historyRunning = false
      publish(channel)
    }
  }

  function retryLine(sessionId: string, branchId: string): void {
    const channel = channels.get(sessionId)
    if (channel === undefined || branchId === MAIN_BRANCH) return
    const branch = channel.branches.get(branchId)
    if (branch === undefined) return
    branch.error = ""
    branch.done = false
    branch.syncing = true
    if (!channel.historyQueue.includes(branchId)) channel.historyQueue.push(branchId)
    publish(channel)
    void pumpHistory(channel)
  }

  function stateOf(channel: Channel, branchId: string): { running: boolean; stopping: boolean } {
    return channel.states.get(branchId) ?? idleState()
  }

  function anyRunning(channel: Channel): boolean {
    const ended = new Set(channel.lines.views().filter(line => line.state.kind.startsWith("ended") || line.state.kind === "creation-failed").map(line => line.id))
    return [...channel.states].some(([id, state]) => !ended.has(id) && (state.running || state.stopping))
  }

  function changed(channel: Channel): void {
    channel.publishTimer ??= setTimeout(() => publish(channel), PUBLISH_INTERVAL_MS)
  }

  function publish(channel: Channel): void {
    clearTimeout(channel.publishTimer)
    channel.publishTimer = undefined
    const view = channel.view
    const main = stateOf(channel, MAIN_BRANCH)
    view.rows = channel.main.projection.rows.slice()
    view.lastTurnEnd = channel.main.projection.lastTurnEnd
    view.earlier = channel.main.before !== undefined
    view.loadingEarlier = channel.main.loadingEarlier
    view.historyError = channel.main.historyError
    view.running = main.running
    view.stopping = main.stopping
    view.retry = channel.main.progress?.retry
    view.sessionRunning = anyRunning(channel)
    view.model = channel.main.projection.model ?? useSessionsStore().byId.get(view.sessionId)?.model
    view.lines = channel.lines.views()
    view.goal = channel.main.projection.goal
    view.queued = channel.main.projection.queued()
    view.questions = [
      ...channel.main.projection.waiting().map((ask) => ({ branchId: MAIN_BRANCH, ...ask })),
      ...[...channel.branches].flatMap(([id, branch]) => branch.projection.waiting().map((ask) => ({ branchId: id, ...ask }))),
    ]
    const branches: Record<string, BranchView> = {}
    for (const [id, branch] of channel.branches) {
      const state = stateOf(channel, id)
      branches[id] = {
        rows: branch.projection.rows.slice(),
        running: state.running,
        stopping: state.stopping,
        status: branch.error !== "" ? "failed" : branch.done ? "ready" : "loading",
        error: branch.error,
        lastTurnEnd: branch.projection.lastTurnEnd,
        earlier: branch.before !== undefined, loadingEarlier: branch.loadingEarlier, historyError: branch.historyError,
      }
    }
    view.branches = branches
  }

  /** A finished turn changes the directory's preview and activity time. */
  function refreshDirectorySoon(): void {
    clearTimeout(refreshTimer)
    refreshTimer = setTimeout(() => void useSessionsStore().refresh(), 500)
  }

  function removeApproval(view: SessionView, branchId: string, requestId: string): void {
    view.approvals = view.approvals.filter((approval) => approval.branchId !== branchId || approval.requestId !== requestId)
  }

  function ensureLine(sessionId: string, branchId: string): void {
    const channel = channels.get(sessionId)
    if (channel === undefined) return
    if (branchId !== MAIN_BRANCH && !channel.lines.has(branchId)) return
    const branch = ensureBranch(channel, branchId)
    if (!branch.opened) {
      branch.opened = true
      branch.syncing = true
      if (!channel.historyQueue.includes(branchId)) channel.historyQueue.push(branchId)
      void pumpHistory(channel)
    }
    publish(channel)
  }

  async function submit(sessionId: string, branchId: string, text: string, attachments: DraftAttachments = { images: [], pastes: [] }): Promise<void> {
    let body = text
    for (const paste of attachments.pastes) {
      const form = new FormData()
      form.append("file", new Blob([paste], { type: "text/plain" }), "paste.txt")
      const response = await fetch(`/api/sessions/${encodeURIComponent(sessionId)}/files`, { method: "POST", body: form })
      if (!response.ok) throw new Error(`Attachment upload failed (${response.status})`)
      const path = readAttachmentPath(await response.json())
      body = `${body}${body === "" ? "" : "\n"}[Pasted text: ${path} (${paste.split("\n").length} lines)]`
    }
    const images = []
    for (const file of attachments.images) {
      const form = new FormData()
      form.append("file", file)
      const response = await fetch(`/api/sessions/${encodeURIComponent(sessionId)}/files?image=true`, { method: "POST", body: form })
      if (!response.ok) throw new Error(`Image upload failed (${response.status})`)
      const uploaded: unknown = await response.json()
      images.push({ type: "image", source: readUploadedImage(uploaded) })
    }
    const params: Record<string, unknown> = { sessionId, branchId, submissionId: crypto.randomUUID(), mode: "queue" }
    if (attachments.images.length === 0) params.text = body
    else params.user = { content: [...(body === "" ? [] : [{ type: "text", text: body }]), ...images] }
    await client().call("session.submit", params, readAck)
  }

  async function stop(sessionId: string, branchId: string): Promise<void> {
    await client().call("session.stop", { sessionId, branchId, reason: "Stopped from the web workspace" }, readAck)
  }

  async function createLine(sessionId: string, parentBranchId: string, at: number, prompt: string): Promise<string> {
    const opened = await client().call("session.branch_create", {
      sessionId,
      parentBranchId,
      at,
      prompt,
    }, readBranchCreate)
    return opened.branchId
  }

  async function endLine(sessionId: string, branchId: string): Promise<void> {
    await client().call("session.branch_end", { sessionId, branchId }, readAck)
  }

  async function memberApproval(sessionId: string, approval: ApprovalRequest) {
    return client().call("session.collaboration.approval.get", { sessionId, branchId: approval.branchId, requestId: approval.requestId }, readMemberApproval)
  }

  async function respond(sessionId: string, approval: ApprovalRequest, decision: ApprovalDecision): Promise<void> {
    await client().call("approval.respond", {
      sessionId,
      branchId: approval.branchId,
      requestId: approval.requestId,
      decision,
    }, readAck)
    const view = views[sessionId]
    if (view !== undefined) removeApproval(view, approval.branchId, approval.requestId)
  }

  /** Allows the approval and, for a simple command, saves an exact allow rule first. */
  async function allowAlways(sessionId: string, approval: ApprovalRequest): Promise<void> {
    const command = alwaysCommand(approval)
    if (command === undefined) throw new Error("This approval has no simple command to allow")
    const rule = alwaysRule(command)
    const rules = await client().call("permission.rules", {}, readRules)
    if (!rules.commandRules.some((existing) => sameRule(existing, rule))) {
      await client().call("permission.set_rules", { expectedRevision: rules.revision, commandRules: [...rules.commandRules, rule] }, readRevision)
    }
    await respond(sessionId, approval, "allow")
  }

  /** Saves a session grant for the approval's file, then allows it. */
  async function allowForSession(sessionId: string, approval: ApprovalRequest): Promise<void> {
    const grant = fileGrant(approval)
    if (grant === undefined) throw new Error("This approval names no file to allow")
    await client().call("session.grants.add", { sessionId, ...grant }, readAck)
    await respond(sessionId, approval, "allow")
  }

  /** Answers a waiting question; `undefined` declines it. The prompt closes when the batch's result arrives. */
  async function answerQuestion(sessionId: string, question: PendingQuestion, answers: QuestionAnswer[] | undefined): Promise<void> {
    await client().call("session.question.respond", {
      sessionId,
      branchId: question.branchId,
      requestId: question.requestId,
      callId: question.callId,
      ...(answers === undefined ? { declined: true } : { answers }),
    }, readAck)
  }

  /** The Session's method table; a daemon without it offers no session commands, and an unreadable table offers none either. */
  async function sessionMethods(sessionId: string): Promise<string[]> {
    if (!useConnectionStore().supports("session.methods")) return []
    return client().call("session.methods", { sessionId }, readSessionMethods).catch(() => [])
  }

  /** Compacts the main line's context. */
  async function compact(sessionId: string) {
    return client().call("session.context.compact", { sessionId, branchId: MAIN_BRANCH }, readCompactResult)
  }

  /** Writes the main line's goal through `session.goal.<call>`; the goal changes when its events arrive. */
  async function writeGoal(sessionId: string, call: GoalCall, objective: string | undefined): Promise<void> {
    await client().call(`session.goal.${call}`, { sessionId, branchId: MAIN_BRANCH, ...(objective === undefined ? {} : { objective }) }, readAck)
    const channel = channels.get(sessionId)
    if (channel !== undefined) {
      await refreshGoal(channel)
      if (channel.view.status === "failed") throw new Error(channel.view.error)
    }
  }

  /** Starts a loop on the main line and returns its id. */
  async function startLoop(sessionId: string, prompt: string, everySeconds: number | undefined): Promise<string> {
    return await client().call("session.loop.start", {
      sessionId,
      branchId: MAIN_BRANCH,
      prompt,
      ...(everySeconds === undefined ? {} : { everySeconds }),
    }, readStartedLoop)
  }

  /** The main line's loops, newest first. */
  async function readLoops(sessionId: string): Promise<Loop[]> {
    return [...await client().call("session.loop.list", { sessionId, branchId: MAIN_BRANCH }, readLoopsReply)]
  }

  /** Stops one loop of the main line. */
  async function stopLoop(sessionId: string, loopId: string): Promise<void> {
    await client().call("session.loop.stop", { sessionId, loopId }, readAck)
  }

  /** Reads the main line's context window usage. */
  async function usage(sessionId: string) {
    return client().call("session.context.usage", { sessionId, branchId: MAIN_BRANCH }, readContextUsage)
  }

  /** Forks the Session at `at` (a main-line cut) and returns the new Session's id. */
  async function fork(sessionId: string, at: number): Promise<string> {
    return client().call("session.fork", { sessionId, at }, readForkedSessionId)
  }

  /** The files an `@` mention offers under the Session's workspace. */
  async function searchFiles(sessionId: string, query: string): Promise<FileCandidate[]> {
    return client().call("session.files.search", { sessionId, query, limit: 20 }, readFileCandidates)
  }

  /** Writes the main line's reasoning effort through `session.model.update`'s preferences; returns the server's outcome. */
  /** Withdraws a queued message; with `steer`, the same content joins the running turn at its next boundary. */
  async function withdrawInput(sessionId: string, inputId: string, steer: UserInput | undefined): Promise<WithdrawResult> {
    return client().call("session.input.withdraw", { sessionId, branchId: MAIN_BRANCH, inputId, ...(steer === undefined ? {} : { steer }) }, readWithdrawResult)
  }

  /** The policy of `main`, or undefined when the Session serves no permission method or the read fails. */
  async function sessionPermission(sessionId: string): Promise<PermissionPolicy | undefined> {
    if (!useConnectionStore().supports("session.permission.get")) return undefined
    return client().call("session.permission.get", { sessionId, branchId: MAIN_BRANCH }, readPermissionState)
      .then((state) => state.policy, () => undefined)
  }

  /** Changes the preset, preserving tool exceptions; a conflict refreshes without retrying. */
  async function setPermission(sessionId: string, preset: PermissionPreset): Promise<PermissionOutcome> {
    const state = await client().call("session.permission.get", { sessionId, branchId: MAIN_BRANCH }, readPermissionState)
    const view = views[sessionId]
    if (view !== undefined) view.permission = state.policy
    if (state.policy.preset === preset) return "unchanged"
    const policy = { ...state.policy, preset }
    try {
      await client().call("session.permission.update", {
        sessionId,
        branchId: MAIN_BRANCH,
        expectedVersion: state.version,
        policy,
      }, readPermissionVersion)
    } catch (cause) {
      if (cause instanceof RpcError && cause.appError === "permission_conflict") {
        if (view !== undefined) view.permission = await sessionPermission(sessionId)
        return "conflict"
      }
      throw cause
    }
    if (view !== undefined) view.permission = policy
    return "updated"
  }

  async function setEffort(sessionId: string, effort: string): Promise<string> {
    return client().call("session.model.update", { sessionId, branchId: MAIN_BRANCH, modelPreferences: { reasoningEffort: effort } }, readConfigurationOutcome)
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
    channel.main.projection.model = result.model
    publish(channel)
  }

  return { views, open, resync, receive, ensureLine, loadEarlier, retryLine, submit, stop, createLine, endLine, respond, allowAlways, allowForSession, answerQuestion, compact, usage, fork, writeGoal, startLoop, readLoops, stopLoop, setEffort, setPermission, withdrawInput, searchFiles, memberApproval, setModel }
})
