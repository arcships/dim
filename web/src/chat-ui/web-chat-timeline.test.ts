import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { computed, createApp, defineComponent, h, nextTick, ref, type App } from "vue"
import { createPinia, setActivePinia, type Pinia } from "pinia"
import { createMemoryHistory, createRouter, type Router } from "vue-router"
import type { ChatUiHost } from "@dimcode/chat-ui"
import { i18n } from "../i18n"
import { Projection } from "../projection"
import type { ApprovalRequest, PendingQuestion, SessionEvent } from "../rpc/protocol"
import { useChatStore } from "../stores/chat"
import { useConnectionStore } from "../stores/connection"
import { useSessionsStore } from "../stores/sessions"
import { createRowMapper } from "./rows"
import { createWebChatUiHost } from "./web-host"
import WebChatTimeline from "./WebChatTimeline.vue"

function event(sequence: number, content: SessionEvent["content"]): SessionEvent {
  return { id: `e${sequence}`, sequence, createAt: 1_700_000_000_000 + sequence * 1000, content } as SessionEvent
}

/** A completed turn with reasoning, a tool call and an answer; a child line; then a turn that fails. */
const recorded: SessionEvent[] = [
  event(1, { kind: "input", text: "read the entry point", content: [{ type: "text", text: "read the entry point" }], blocks: [] }),
  event(2, { kind: "thinking", requestId: "r1", delta: "open main first" }),
  event(3, { kind: "tool-use", requestId: "r1", callId: "call-1", name: "read", input: { path: "src/main.rs" } }),
  event(4, { kind: "tool-completed", requestId: "r1", callId: "call-1", result: { text: "fn main() {}", isError: false } }),
  event(5, { kind: "text", requestId: "r1", delta: "The entry point only defines main." }),
  event(6, { kind: "model-end", requestId: "r1" }),
  event(7, { kind: "turn-end", outcome: "completed" }),
  event(8, { kind: "branch-create", branchId: "b-7c0ffee1", at: 0, prompt: "parser race", source: { kind: "host", operationId: "op" } }),
  event(9, { kind: "input", text: "now run /skill:commit it", content: [{ type: "text", text: "now run /skill:commit it" }], blocks: [] }),
  event(10, { kind: "model-end", requestId: "r2", error: "boom", failure: { kind: "server_error", status: 503, cause: [], attempts: 1, waitedMs: 0 } }),
  event(11, { kind: "turn-end", outcome: "failed", error: "boom" }),
]

const UIcon = defineComponent({ name: "UIcon", props: { name: String }, setup: props => () => h("i", { "data-icon": props.name }) })
const UButton = defineComponent({ name: "UButton", props: { label: String }, setup: (props, { slots }) => () => h("button", { type: "button" }, slots.default?.() ?? props.label) })
/** Nuxt UI's modal, drawn in place while open. */
const UModal = defineComponent({
  name: "UModal",
  props: { open: Boolean },
  setup: (props, { slots }) => () => props.open ? h("div", { "data-testid": "modal" }, [slots.header?.(), slots.footer?.()]) : null,
})

/**
 * What a real App Server announces for the timeline's commands
 * (`crates/dim-app-server/src/transport.rs`): these in hello's features,
 * `session.question.respond` in the Session's method table, and the built-in
 * `approval.respond` in neither.
 */
const HELLO_FEATURES: string[] = ["session.submit", "session.stop", "session.fork", "session.branch_create"]
const SESSION_METHODS: string[] = ["session.input.pending", "session.question.pending", "session.turn.get", "session.question.respond"]

const mounted: App[] = []
// jsdom lays nothing out; the virtualizer needs a viewport size to render a window.
beforeEach(() => {
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(800)
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(600)
})
afterEach(() => {
  while (mounted.length > 0) mounted.pop()!.unmount()
  document.body.replaceChildren()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  vi.useRealTimers()
})

async function settle(): Promise<void> {
  for (let i = 0; i < 5; i++) {
    await nextTick()
    await new Promise(resolve => setTimeout(resolve, 0))
  }
}

function testRouter(): Router {
  const page = { render: () => null }
  return createRouter({ history: createMemoryHistory(), routes: [{ path: "/", component: page }, { path: "/chat/:sessionId", name: "chat-session", component: page }] })
}

/** A pinia whose connection serves `features` and whose chat store has a loaded view of `s-1`. */
/** A pinia whose connection serves `features` and whose chat store has a loaded view of `s-1` unless `view` is null. */
function stores(features: readonly string[] | undefined, view: Record<string, unknown> | null = {}): Pinia {
  const pinia = createPinia()
  setActivePinia(pinia)
  if (features !== undefined) useConnectionStore().hello = { instanceId: "i", serverVersion: "0", protocolVersion: 4, features: [...features] }
  const chat = useChatStore()
  if (view !== null) chat.views["s-1"] = { sessionId: "s-1", status: "ready", approvals: [], questions: [], methods: SESSION_METHODS, ...view } as unknown as (typeof chat.views)[string]
  return pinia
}

type Mounted = { root: HTMLElement; pinia: Pinia; router: Router }

function mountWith(rows: Projection["rows"], options: { features?: readonly string[]; view?: null; props?: Record<string, unknown> } = {}): Mounted {
  const container = document.createElement("div")
  document.body.append(container)
  const pinia = stores(options.features, options.view)
  const router = testRouter()
  const app = createApp({
    render: () => h(WebChatTimeline, { sessionId: "s-1", branchId: "main", rows, running: false, quotable: true, ...options.props }),
  })
  app.use(pinia).use(router).use(i18n).component("UIcon", UIcon).component("UButton", UButton).component("UModal", UModal)
  app.mount(container)
  mounted.push(app)
  return { root: container, pinia, router }
}

function mount(rows: Projection["rows"], onOpenLine: (branchId: string) => void, extra: Record<string, unknown> = {}): HTMLElement {
  return mountWith(rows, { props: { onOpenLine, ...extra } }).root
}

function project(events: readonly SessionEvent[]): Projection {
  const projection = new Projection()
  for (const item of events) projection.apply(item)
  return projection
}

function userActions(root: HTMLElement, index: number): HTMLElement {
  return root.querySelectorAll<HTMLElement>(".web-user-message")[index]!
}

describe("WebChatTimeline", () => {
  it("renders recorded session events through the Projection and the shared timeline", async () => {
    const projection = new Projection()
    for (const item of recorded) projection.apply(item)
    expect(projection.rows.map(row => row.kind)).toEqual(["user", "thinking", "tool", "assistant", "line", "user", "error"])

    const rows = createRowMapper()(projection.rows, "s-1")
    expect(rows.map(row => row.kind)).toEqual(["turn", "message", "turn"])

    const openLine = vi.fn()
    const root = mount(projection.rows, openLine)
    await settle()

    const bubbles = [...root.querySelectorAll(".timeline-user-bubble")].map(bubble => bubble.textContent?.trim())
    expect(bubbles).toEqual(["read the entry point", "now run /skill:commit it"])
    expect([...root.querySelectorAll(".timeline-user-bubble .skill-chip")].map(chip => chip.textContent)).toEqual(["/skill:commit"])
    expect(root.querySelector("[data-testid='timeline-text-block']")?.textContent).toContain("The entry point only defines main.")

    root.querySelector<HTMLButtonElement>("[data-testid='timeline-work-summary'] button")!.click()
    await settle()
    root.querySelector<HTMLButtonElement>("[data-testid='timeline-block-group-toggle']")!.click()
    await settle()
    expect(root.querySelector("[data-testid='timeline-thinking-block']")?.textContent).toContain("open main first")
    const tool = root.querySelector("[data-testid='timeline-tool-block']")
    expect(tool?.textContent).toContain("Read")
    expect(tool?.textContent).toContain("main.rs")

    const failure = root.querySelector("[data-testid='timeline-terminal-line']")
    expect(failure?.textContent).toContain("The provider reported a server error")

    for (const id of ["timeline-user-copy", "timeline-assistant-copy"]) {
      const button = root.querySelector(`[data-testid='${id}']`)
      expect(button?.getAttribute("aria-label"), id).toBeTruthy()
    }

    const line = root.querySelector<HTMLButtonElement>(".timeline-line-marker")
    expect(line?.textContent).toContain("parser race")
    line!.click()
    expect(openLine).toHaveBeenCalledWith("b-7c0ffee1")
  })

  it("copies the assistant answer through the Web clipboard fallback on an insecure origin", async () => {
    const projection = new Projection()
    for (const item of recorded.slice(0, 7)) projection.apply(item)
    const writeText = vi.fn(async () => undefined)
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } })
    vi.stubGlobal("isSecureContext", false)
    const copied: string[] = []
    const execCommand = vi.fn((command: string) => {
      copied.push(`${command}:${document.querySelector("textarea")?.value}`)
      return true
    })
    Object.defineProperty(document, "execCommand", { configurable: true, value: execCommand })

    const root = mount(projection.rows, vi.fn())
    await settle()
    root.querySelector<HTMLButtonElement>("[data-testid='timeline-assistant-copy']")!.click()
    await settle()

    expect(copied).toEqual(["copy:The entry point only defines main."])
    expect(writeText).not.toHaveBeenCalled()
  })

  it("keeps a provider failure and a different turn failure both visible, each with its details", async () => {
    const projection = new Projection()
    projection.apply(event(1, { kind: "input", text: "call the provider", content: [{ type: "text", text: "call the provider" }], blocks: [] }))
    projection.apply(event(2, { kind: "model-end", requestId: "r1", error: "HTTP 503", failure: { kind: "server_error", status: 503, providerCode: "overloaded", cause: ["upstream unavailable"], attempts: 1, waitedMs: 0 } }))
    projection.apply(event(3, { kind: "turn-end", outcome: "failed", error: "Outer turn failure" }))
    expect(projection.rows.map(row => row.kind)).toEqual(["user", "error", "error"])

    const root = mount(projection.rows, () => {})
    await settle()
    const lines = [...root.querySelectorAll("[data-testid='timeline-terminal-line']")]
    expect(lines.map(line => line.querySelector("[data-testid='timeline-terminal-label']")?.textContent?.trim())).toEqual([
      expect.stringContaining("The provider reported a server error"),
      "Outer turn failure",
    ])

    const toggle = lines[0]!.querySelector<HTMLButtonElement>("[data-testid='timeline-terminal-toggle']")!
    toggle.click()
    await settle()
    expect(toggle.getAttribute("aria-expanded")).toBe("true")
    expect(lines[0]!.textContent).toContain("status: 503")
    expect(lines[0]!.textContent).toContain("provider code: overloaded")
  })

  it("shows a turn failure that repeats the provider failure once", async () => {
    const projection = new Projection()
    projection.apply(event(1, { kind: "input", text: "call the provider", content: [{ type: "text", text: "call the provider" }], blocks: [] }))
    projection.apply(event(2, { kind: "model-end", requestId: "r1", error: "HTTP 503", failure: { kind: "server_error", status: 503, cause: [], attempts: 1, waitedMs: 0 } }))
    projection.apply(event(3, { kind: "turn-end", outcome: "failed", error: "HTTP 503" }))

    const root = mount(projection.rows, () => {})
    await settle()
    const lines = root.querySelectorAll("[data-testid='timeline-terminal-line']")
    expect(lines).toHaveLength(1)
    expect(lines[0]!.textContent).toContain("The provider reported a server error")
  })

  it("announces a retry once per attempt while its visible countdown ticks", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"], now: 1_700_000_000_000 })
    const projection = new Projection()
    projection.apply(recorded[0]!)
    const retry = { attempt: 2, maxAttempts: 8, retryAtMs: Date.now() + 5_000, kind: "server_error", reason: "overloaded" }
    const root = mount(projection.rows, () => {}, { running: true, retry })
    await settle()
    const live = (): string[] => [...root.querySelectorAll("[aria-live]")]
      .filter(region => region.closest("[aria-hidden='true']") === null)
      .map(region => region.textContent?.trim() ?? "")
    const countdown = (): string | undefined => root.querySelector("[data-testid='web-chat-retry']")?.textContent ?? undefined

    const before = live()
    const shown = countdown()
    expect(before).toContain("Retrying, attempt 2 of 8: The provider reported a server error")
    expect(shown).toContain("in 5 s")
    vi.advanceTimersByTime(2_000)
    await settle()
    expect(countdown()).toContain("in 3 s")
    expect(live()).toEqual(before)
    expect(root.querySelector("[data-testid='web-chat-retry']")?.closest("[aria-hidden='true']")).not.toBeNull()
  })

  it("rebuilds only the turn whose rows changed", () => {
    const projection = new Projection()
    for (const item of recorded.slice(0, 7)) projection.apply(item)
    projection.apply(event(9, { kind: "input", text: "again", content: [{ type: "text", text: "again" }], blocks: [] }))
    projection.apply(event(10, { kind: "text", requestId: "r2", delta: "part" }))
    const map = createRowMapper()
    const before = map(projection.rows.slice(), "s-1")
    projection.apply(event(11, { kind: "text", requestId: "r2", delta: "ial" }))
    const after = map(projection.rows.slice(), "s-1")
    expect(after[0]).toBe(before[0])
    expect(after[1]).not.toBe(before[1])
    expect(after[1]?.kind === "turn" && after[1].turn.blocks[0]?.content).toBe("partial")
  })
})

describe("WebChatTimeline actions", () => {
  it("branches from an older message of the main chat at that message's cut", async () => {
    const onBranch = vi.fn()
    const { root } = mountWith(project(recorded).rows, { features: HELLO_FEATURES, props: { branchable: true, onBranch } })
    await settle()
    const branch = userActions(root, 0).querySelector<HTMLButtonElement>("[data-testid='timeline-user-branch']")!
    expect(branch.disabled).toBe(false)
    expect(branch.getAttribute("aria-label")).toBe("Branch from here")
    branch.click()
    await settle()
    expect(onBranch).toHaveBeenCalledWith("read the entry point", 0)
  })

  it("branches from an older message of a line at that message's cut", async () => {
    const onBranch = vi.fn()
    const line = project([
      event(4, { kind: "input", text: "first on the line", content: [{ type: "text", text: "first on the line" }], blocks: [] }),
      event(5, { kind: "text", requestId: "r1", delta: "ok" }),
      event(6, { kind: "model-end", requestId: "r1" }),
      event(7, { kind: "turn-end", outcome: "completed" }),
      event(8, { kind: "input", text: "second on the line", content: [{ type: "text", text: "second on the line" }], blocks: [] }),
    ])
    const { root } = mountWith(line.rows, { features: HELLO_FEATURES, props: { branchId: "b-7c0ffee1", variant: "panel", branchable: true, onBranch } })
    await settle()
    expect(userActions(root, 0).querySelector("[data-testid='timeline-user-fork']"), "a fork copies the main line only").toBeNull()
    userActions(root, 0).querySelector<HTMLButtonElement>("[data-testid='timeline-user-branch']")!.click()
    await settle()
    expect(onBranch).toHaveBeenCalledWith("first on the line", 3)
  })

  it("keeps branching disabled, with its reason, while the line is not branchable", async () => {
    const onBranch = vi.fn()
    const { root } = mountWith(project(recorded).rows, { features: HELLO_FEATURES, props: { branchable: false, onBranch } })
    await settle()
    const branch = userActions(root, 0).querySelector<HTMLButtonElement>("[data-testid='timeline-user-branch']")!
    expect(branch.disabled).toBe(true)
    expect(branch.getAttribute("aria-label")).toBe("Branching waits until the pending prompt is answered, and an ended line cannot branch.")
    branch.click()
    await settle()
    expect(onBranch).not.toHaveBeenCalled()
  })

  /** A user message written at `sequence`, its `input/message` event id `e<sequence>`. */
  const say = (sequence: number, text: string) => event(sequence, { kind: "input", text, content: [{ type: "text", text }], blocks: [] })
  /** The queue's release of the input written at `input`. */
  const applied = (sequence: number, input: number) => event(sequence, { kind: "input-applied", inputId: `e${input}` })
  /** Loop's take of the input written at `input` into the request it starts. */
  const consumed = (sequence: number, input: number) => event(sequence, { kind: "input-consumed", inputId: `e${input}` })
  /** A finished first turn: its message applied and consumed, an answer, the turn end at 7. */
  const finishedTurn = [
    say(1, "read the entry point"),
    applied(2, 1),
    consumed(3, 1),
    event(4, { kind: "text", requestId: "r1", delta: "done" }),
    event(5, { kind: "model-end", requestId: "r1" }),
    event(7, { kind: "turn-end", outcome: "completed" }),
  ]
  const notice = (sequence: number) => event(sequence, { kind: "branch-create", branchId: "b-1", at: 7, prompt: "side quest", source: { kind: "tool", turnId: "t", requestId: "r", callId: "c" } })

  /** The user messages' branch and fork state, by message text. */
  async function actionsOf(events: readonly SessionEvent[], running = true): Promise<Record<string, { branch: string | null; fork: string | null }>> {
    const projection = project(events)
    const { root } = mountWith(projection.rows, { features: HELLO_FEATURES, props: { running, branchable: true } })
    await settle()
    const state = (button: HTMLButtonElement | null): string | null => button === null ? null : button.disabled ? `disabled: ${button.getAttribute("aria-label")}` : "enabled"
    return Object.fromEntries([...root.querySelectorAll<HTMLElement>(".web-user-message")].map(message => [
      message.querySelector(".timeline-user-bubble")!.textContent!.trim(),
      { branch: state(message.querySelector("[data-testid='timeline-user-branch']")), fork: state(message.querySelector("[data-testid='timeline-user-fork']")) },
    ]))
  }
  const running = { branch: "disabled: You can branch from here once this turn finishes.", fork: "disabled: You can fork from here once this turn finishes." }
  const open = { branch: "enabled", fork: "enabled" }

  it("keeps the running turn's message from branching or forking when a notice splits the turn", async () => {
    const events = [...finishedTurn, say(9, "keep going"), applied(10, 9), consumed(11, 9), notice(12), event(13, { kind: "text", requestId: "r2", delta: "still working" })]
    expect(createRowMapper()(project(events).rows, "s-1").map(row => row.kind)).toEqual(["turn", "turn", "message", "turn"])
    expect(await actionsOf(events)).toEqual({ "read the entry point": open, "keep going": running })
  })

  it("keeps a message steered or queued before the turn's first output from branching or forking", async () => {
    expect(await actionsOf([...finishedTurn, say(8, "start the run"), applied(9, 8), say(10, "and also this")])).toEqual({
      "read the entry point": open,
      "start the run": running,
      "and also this": running,
    })
  })

  it("follows a queued message into its own turn after the turn it waited behind ends", async () => {
    const whileHeld = [
      ...finishedTurn,
      say(8, "first"),
      applied(9, 8),
      consumed(10, 8),
      say(11, "queued next"),
      event(12, { kind: "text", requestId: "r2", delta: "answering first" }),
    ]
    expect(await actionsOf(whileHeld), "the queued message is held during the first one's turn").toEqual({ "read the entry point": open, first: running, "queued next": running })
    const ownTurn = [
      ...whileHeld,
      event(13, { kind: "model-end", requestId: "r2" }),
      event(14, { kind: "turn-end", outcome: "completed" }),
      applied(15, 11),
      consumed(16, 11),
      notice(17),
      event(18, { kind: "text", requestId: "r3", delta: "answering the queued one" }),
    ]
    expect(await actionsOf(ownTurn)).toEqual({ "read the entry point": open, first: open, "queued next": running })
  })

  it("keeps a steer a stop left unconsumed in the turn that consumes it", async () => {
    const stopped = [
      ...finishedTurn,
      say(8, "run a tool"),
      applied(9, 8),
      consumed(10, 8),
      event(11, { kind: "tool-use", requestId: "r2", callId: "call-2", name: "exec", input: { command: "sleep 30" } }),
      say(12, "steer this"),
      applied(13, 12),
      event(14, { kind: "tool-completed", requestId: "r2", callId: "call-2", result: { text: "interrupted", isError: true } }),
      event(15, { kind: "turn-end", outcome: "cancelled" }),
    ]
    const nextTurn = [...stopped, say(16, "next"), applied(17, 16), consumed(18, 12), consumed(19, 16), event(20, { kind: "text", requestId: "r3", delta: "taking both" })]
    expect(await actionsOf(nextTurn)).toEqual({ "read the entry point": open, "run a tool": open, "steer this": running, next: running })
    const ended = [...nextTurn, event(21, { kind: "model-end", requestId: "r3" }), event(22, { kind: "turn-end", outcome: "completed" })]
    expect(await actionsOf(ended, false)).toEqual({ "read the entry point": open, "run a tool": open, "steer this": open, next: open })
  })

  it("settles a withdrawn message", async () => {
    const events = [...finishedTurn, say(8, "first"), applied(9, 8), consumed(10, 8), say(11, "withdrawn"), event(12, { kind: "input-deleted", inputId: "e11" }), event(13, { kind: "turn-end", outcome: "completed" }), say(14, "next"), applied(15, 14)]
    expect(await actionsOf(events)).toEqual({ "read the entry point": open, first: open, withdrawn: open, next: running })
  })

  it("forks the main line before a message after confirmation and opens the new session", async () => {
    const { root, pinia, router } = mountWith(project(recorded).rows, { features: HELLO_FEATURES, props: { branchable: true } })
    const fork = vi.spyOn(useChatStore(pinia), "fork").mockResolvedValue("s-2")
    vi.spyOn(useSessionsStore(pinia), "refresh").mockResolvedValue()
    await settle()
    userActions(root, 1).querySelector<HTMLButtonElement>("[data-testid='timeline-user-fork']")!.click()
    await settle()
    const modal = root.ownerDocument.querySelector("[data-testid='modal']")!
    expect(modal.textContent).toContain("Fork the session before “now run /skill:commit it”?")
    expect(fork).not.toHaveBeenCalled()
    ;[...modal.querySelectorAll("button")].find(button => button.textContent === "Fork")!.click()
    await settle()
    expect(fork).toHaveBeenCalledWith("s-1", 8)
    expect(router.currentRoute.value.fullPath).toBe("/chat/s-2")
  })

  it("quotes a message into the composer from its action row", async () => {
    const onQuote = vi.fn()
    const { root } = mountWith(project(recorded).rows, { features: HELLO_FEATURES, props: { onQuote } })
    await settle()
    userActions(root, 1).querySelector<HTMLButtonElement>("[data-testid='web-user-quote']")!.click()
    expect(onQuote).toHaveBeenCalledWith("now run /skill:commit it")
  })

  it("shows the actions the web cannot run disabled, each with its reason", async () => {
    const { root } = mountWith(project(recorded).rows, { features: HELLO_FEATURES, props: { branchable: true } })
    await settle()
    const edit = userActions(root, 0).querySelector<HTMLButtonElement>("[data-testid='web-user-edit']")!
    expect(edit.disabled).toBe(true)
    expect(edit.getAttribute("aria-label")).toBe("Editing a sent message is not available in the web workspace yet.")
    const retry = root.querySelector<HTMLButtonElement>("[data-testid='timeline-assistant-retry']")!
    expect(retry.disabled).toBe(true)
    expect(retry.getAttribute("aria-label")).toBe("Retrying a turn is not available in the web workspace yet.")
  })

  it("names the missing method when the server does not serve branching", async () => {
    const { root } = mountWith(project(recorded).rows, { features: HELLO_FEATURES.filter(method => method !== "session.branch_create"), props: { branchable: true } })
    await settle()
    const branch = userActions(root, 0).querySelector<HTMLButtonElement>("[data-testid='timeline-user-branch']")!
    expect(branch.disabled).toBe(true)
    expect(branch.getAttribute("aria-label")).toBe("This server does not offer session.branch_create.")
  })

  it("stays read-only while its session is not loaded", async () => {
    const { root } = mountWith(project(recorded).rows, { features: HELLO_FEATURES, view: null, props: { branchable: true } })
    await settle()
    for (const id of ["timeline-user-branch", "timeline-user-fork", "web-user-edit", "timeline-assistant-retry"]) {
      expect(root.querySelector(`[data-testid='${id}']`), id).toBeNull()
    }
    expect(root.querySelector("[data-testid='web-user-quote']")).not.toBeNull()
  })

  it("shows an image-only tool result with the session's file URL", async () => {
    const projection = project([
      recorded[0]!,
      event(2, { kind: "tool-use", requestId: "r1", callId: "call-1", name: "screenshot", input: {} }),
      event(3, { kind: "tool-completed", requestId: "r1", callId: "call-1", result: { text: "", images: [{ type: "image", source: { type: "file", mediaType: "image/png", fileId: "f 1" } }], isError: false } }),
      event(4, { kind: "model-end", requestId: "r1" }),
      event(5, { kind: "turn-end", outcome: "completed" }),
    ])
    const { root } = mountWith(projection.rows, { features: HELLO_FEATURES })
    await settle()
    root.querySelector<HTMLButtonElement>("[data-testid='timeline-work-summary'] button")!.click()
    await settle()
    root.querySelector<HTMLElement>("[data-testid='timeline-tool-block'] [role='button'], [data-testid='timeline-tool-block'] button")!.click()
    await settle()
    const images = [...root.querySelectorAll<HTMLImageElement>("[data-testid='web-tool-image']")]
    expect(images.map(image => image.getAttribute("src"))).toEqual(["/api/sessions/s-1/files/f%201"])
    expect(root.querySelector("[data-testid='web-tool-result'] pre")).toBeNull()
  })

  it("unfolds the images of a read result in place of the file preview", async () => {
    const projection = project([
      recorded[0]!,
      event(2, { kind: "tool-use", requestId: "r1", callId: "call-1", name: "read", input: { path: "pic.png" } }),
      event(3, { kind: "tool-completed", requestId: "r1", callId: "call-1", result: { text: "pic.png: 64x40", images: [{ type: "image", source: { type: "file", mediaType: "image/png", fileId: "f-2" } }], isError: false } }),
      event(4, { kind: "model-end", requestId: "r1" }),
      event(5, { kind: "turn-end", outcome: "completed" }),
    ])
    const { root } = mountWith(projection.rows, { features: HELLO_FEATURES })
    await settle()
    root.querySelector<HTMLButtonElement>("[data-testid='timeline-work-summary'] button")!.click()
    await settle()
    root.querySelector<HTMLElement>("[data-testid='timeline-tool-block'] [role='button'], [data-testid='timeline-tool-block'] button")!.click()
    await settle()
    expect(root.querySelector("[data-testid='web-tool-result'] pre")?.textContent).toBe("pic.png: 64x40")
    expect(root.querySelector("[data-testid='web-tool-image']")?.getAttribute("src")).toBe("/api/sessions/s-1/files/f-2")
  })
})

describe("Web ChatUiHost commands", () => {
  const approval: ApprovalRequest = { sessionId: "s-1", branchId: "main", requestId: "req-1", call: { name: "exec", input: {} }, reason: "run" }
  const question: PendingQuestion = {
    branchId: "main",
    requestId: "ask-1",
    callId: "call-9",
    questions: [
      { id: "q-pick", question: "Which?", options: [{ label: "Yes" }, { label: "No" }], multiSelect: false, allowCustom: false },
      { id: "q-free", question: "Why?", options: [], multiSelect: false, allowCustom: true },
    ],
  }
  const rows = project(recorded).rows

  function setup(features: readonly string[] = HELLO_FEATURES, methods: readonly string[] = SESSION_METHODS, view: "loaded" | "missing" = "loaded") {
    const pinia = stores(features, view === "missing" ? null : { approvals: [approval], questions: [question], methods })
    const router = testRouter()
    const openBranch = vi.fn()
    const confirmFork = vi.fn(async () => true)
    let host!: ChatUiHost
    const app = createApp({
      setup() {
        host = createWebChatUiHost({
          sessionId: ref("s-1"),
          branchId: ref("main"),
          rows: computed(() => createRowMapper()(rows, "s-1")),
          running: ref(false),
          branchable: ref(true),
          rowByKey: key => rows.find(row => row.key === key),
          noticeText: () => "",
          confirmFork,
          openBranch,
        }).host
        return () => null
      },
    })
    app.use(pinia).use(router).use(i18n).mount(document.createElement("div"))
    mounted.push(app)
    return { host, chat: useChatStore(pinia), sessions: useSessionsStore(pinia), router, openBranch, confirmFork }
  }

  it("maps each supported command to its chat store action", async () => {
    const { host, chat, sessions, router, openBranch, confirmFork } = setup()
    expect(host.capabilities.value.interactive).toBe(true)
    const submit = vi.spyOn(chat, "submit").mockResolvedValue()
    const stop = vi.spyOn(chat, "stop").mockResolvedValue()
    const fork = vi.spyOn(chat, "fork").mockResolvedValue("s-2")
    const respond = vi.spyOn(chat, "respond").mockResolvedValue()
    const answer = vi.spyOn(chat, "answerQuestion").mockResolvedValue()
    vi.spyOn(sessions, "refresh").mockResolvedValue()

    await host.commands.send({ text: "hello", mode: "steer" })
    expect(submit).toHaveBeenCalledWith("s-1", "main", "hello", undefined, "steer")
    await host.commands.stop()
    expect(stop).toHaveBeenCalledWith("s-1", "main")
    await host.commands.fork("user-9")
    expect(confirmFork).toHaveBeenCalledWith(expect.stringContaining("now run /skill:commit it"))
    expect(fork).toHaveBeenCalledWith("s-1", 8)
    expect(router.currentRoute.value.params.sessionId).toBe("s-2")
    await host.commands.branch("user-1")
    expect(openBranch).toHaveBeenCalledWith("read the entry point", 0)
    await host.commands.approve("req-1", "deny")
    expect(respond).toHaveBeenCalledWith("s-1", approval, "deny")
    await host.commands.answer("ask-1", [{ questionId: "q-pick", answer: "Yes" }, { questionId: "q-free", answer: "because" }])
    expect(answer).toHaveBeenCalledWith("s-1", question, [{ questionId: "q-pick", selected: ["Yes"] }, { questionId: "q-free", selected: [], custom: "because" }])
  })

  it("rejects what it cannot run without calling the store", async () => {
    const { host, chat } = setup(HELLO_FEATURES.filter(method => method !== "session.stop"))
    const stop = vi.spyOn(chat, "stop")
    const answer = vi.spyOn(chat, "answerQuestion")
    await expect(host.commands.stop()).rejects.toThrow("This server does not offer session.stop.")
    expect(stop).not.toHaveBeenCalled()
    await expect(host.commands.retry("user-1")).rejects.toThrow("Retrying a turn is not available in the web workspace yet.")
    await expect(host.commands.edit("user-1", "x")).rejects.toThrow("Editing a sent message is not available in the web workspace yet.")
    await expect(host.commands.send({ text: "x", mode: "append" })).rejects.toThrow("The web workspace sends a message queued or steered, not appended.")
    await expect(host.commands.approve("req-gone", "allow")).rejects.toThrow("That request is no longer waiting.")
    await expect(host.commands.answer("ask-1", [{ questionId: "q-pick", answer: "Maybe" }])).rejects.toThrow("“Maybe” is not an option of this question.")
    expect(answer).not.toHaveBeenCalled()
  })

  it("approves through the built-in approval.respond, which hello does not announce", async () => {
    const { host, chat } = setup([])
    expect(host.capabilities.value.interactive).toBe(true)
    const respond = vi.spyOn(chat, "respond").mockResolvedValue()
    await host.commands.approve("req-1", "allow")
    expect(respond).toHaveBeenCalledWith("s-1", approval, "allow")
  })

  it("answers only when the Session serves session.question.respond", async () => {
    const { host, chat } = setup(HELLO_FEATURES, SESSION_METHODS.filter(method => method !== "session.question.respond"))
    const answer = vi.spyOn(chat, "answerQuestion")
    await expect(host.commands.answer("ask-1", [{ questionId: "q-pick", answer: "Yes" }])).rejects.toThrow("This session does not offer session.question.respond.")
    expect(answer).not.toHaveBeenCalled()
  })

  it("is not interactive while its session is not loaded", async () => {
    const { host, chat } = setup(HELLO_FEATURES, SESSION_METHODS, "missing")
    expect(host.capabilities.value.interactive).toBe(false)
    const respond = vi.spyOn(chat, "respond")
    await expect(host.commands.approve("req-1", "allow")).rejects.toThrow("The session is not loaded.")
    expect(respond).not.toHaveBeenCalled()
  })
})
