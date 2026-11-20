import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { createApp, defineComponent, h, nextTick, type App } from "vue"
import { createPinia } from "pinia"
import { i18n } from "../i18n"
import { Projection } from "../projection"
import type { SessionEvent } from "../rpc/protocol"
import { createRowMapper } from "./rows"
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
const UButton = defineComponent({ name: "UButton", setup: (_, { slots }) => () => h("button", { type: "button" }, slots.default?.()) })

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
  vi.useRealTimers()
})

async function settle(): Promise<void> {
  for (let i = 0; i < 5; i++) {
    await nextTick()
    await new Promise(resolve => setTimeout(resolve, 0))
  }
}

function mount(rows: Projection["rows"], onOpenLine: (branchId: string) => void, extra: Record<string, unknown> = {}): HTMLElement {
  const container = document.createElement("div")
  document.body.append(container)
  const app = createApp({
    render: () => h(WebChatTimeline, { sessionId: "s-1", rows, running: false, quotable: true, onOpenLine, ...extra }),
  })
  app.use(createPinia()).use(i18n).component("UIcon", UIcon).component("UButton", UButton)
  app.mount(container)
  mounted.push(app)
  return container
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
