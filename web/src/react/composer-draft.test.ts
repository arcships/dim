import React, { act, createElement } from "react"
import { createRoot } from "react-dom/client"
import { expect, test, vi } from "vitest"
import type { BranchView, SessionView } from "../stores/chat"
import { Composer } from "./composer"
import { chat } from "./stores"

vi.hoisted(() => vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {}, removeEventListener() {} })))
vi.mock("#workspace-store", () => import("./define-store"))
vi.mock("#workspace-i18n", () => import("./i18n"))

test("leaving and reopening a conversation keeps its complete draft until its own send succeeds", async () => {
  vi.stubGlobal("React", React)
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true)
  const scrollIntoView = HTMLElement.prototype.scrollIntoView
  HTMLElement.prototype.scrollIntoView = vi.fn()
  const line: BranchView = { status: "ready", rows: [], queued: [], running: false, stopping: false,
    error: "", model: undefined, lastTurnEnd: undefined, earlier: false, loadingEarlier: false, historyError: "" }
  const session: SessionView = { ...line, sessionId: "first", defaultBranchId: "main", lines: [], branches: { side: line },
    retry: undefined, sessionRunning: false, methods: ["attachment.put", "session.files.search", "session.input.withdraw"], approvals: [], questions: [],
    delegationRevision: 0, permission: undefined, permissionVersion: undefined, goal: undefined }
  const actions = { newSession: vi.fn(), scheduled: vi.fn() }
  vi.spyOn(chat, "searchFiles").mockResolvedValue([{ relativePath: "notes.md", absolutePath: "/work/notes.md", dir: false }])
  let accept!: () => void
  let reject!: (error: Error) => void
  const send = vi.spyOn(chat, "submit").mockImplementation(() => new Promise<string>((yes, no) => { accept = () => yes("sent"); reject = no }))
  const host = document.body.appendChild(document.createElement("div"))
  const root = createRoot(host)
  const show = (id: string, branchId = "main") => act(async () => root.render(createElement(Composer, {
    key: JSON.stringify([id, branchId]), connected: true, session: { ...session, sessionId: id }, branchId, actions,
  })))
  const field = () => host.querySelector("textarea")!
  const type = (text: string) => act(async () => {
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(field(), text)
    field().dispatchEvent(new Event("input", { bubbles: true }))
  })
  const submit = () => act(async () => { host.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })) })
  try {
    session.running = true
    await show("queue")
    await act(async () => field().focus())
    await type("/")
    expect(host.querySelector('[role="listbox"]')).not.toBeNull()
    const queue = [...host.querySelectorAll("button")].find(button => button.textContent === "Queue")!
    await act(async () => queue.focus())
    expect(host.querySelector('[role="listbox"]')).toBeNull()
    await act(async () => queue.click())
    expect(actions.newSession).not.toHaveBeenCalled()
    expect(send).toHaveBeenCalledExactlyOnceWith("queue", "main", "/", expect.anything(), "queue")
    await act(async () => accept())
    expect(host.querySelector('[role="status"]')?.textContent).toBe("Held for the next turn")
    send.mockClear()
    session.running = false
    await show("first")
    await type("read @note")
    await act(async () => { await vi.waitFor(() => expect(chat.searchFiles).toHaveBeenCalled()) })
    await act(async () => host.querySelector<HTMLElement>('[role="option"]')!.click())
    const image = new File(["image"], "photo.png", { type: "image/png" })
    const paste = "long text\n".repeat(2048)
    await act(async () => {
      const picker = host.querySelector<HTMLInputElement>('[type="file"]')!
      Object.defineProperty(picker, "files", { value: [image] })
      picker.dispatchEvent(new Event("change", { bubbles: true }))
    })
    await act(async () => {
      field().dispatchEvent(Object.assign(new Event("paste", { bubbles: true, cancelable: true }), {
        clipboardData: { files: [], getData: () => paste },
      }))
    })
    const recalled = { type: "image", source: { type: "file", mediaType: "image/png", fileId: "queued-file" } } as const
    session.running = true
    session.queued = [{ id: "held", text: "queued", user: { content: [recalled] }, content: [{ type: "text", text: "queued" }, recalled] }]
    vi.spyOn(chat, "withdrawInput").mockResolvedValue({ status: "deleted" })
    let stopped!: () => void
    vi.spyOn(chat, "stop").mockImplementation(() => new Promise<void>(resolve => { stopped = resolve }))
    await show("first")
    await act(async () => { field().dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true })) })
    await act(async () => root.render(null))
    await act(async () => stopped())
    expect(send).not.toHaveBeenCalled()
    session.running = false
    session.queued = []
    await show("first", "side")
    expect(field().value).toBe("")
    await type("side draft")
    await show("second")
    expect(field().value).toBe("")
    await type("second session draft")
    await show("first")
    expect(field().value).toBe("queued\nread @notes.md ")
    expect(host.querySelectorAll(".rw-files li")).toHaveLength(3)
    await submit()
    const attachments = expect.objectContaining({ images: [image], pastes: [paste], restored: [expect.objectContaining({ image: recalled, at: 6 })] })
    const body = "queued\nread [@notes.md](/work/notes.md) "
    expect(send).toHaveBeenCalledWith("first", "main", body, attachments, "queue")
    await show("second")
    expect(field().value).toBe("second session draft")
    await show("first")
    expect(field().readOnly).toBe(true)
    await submit()
    expect(send).toHaveBeenCalledTimes(1)
    await act(async () => reject(new Error("upload interrupted")))
    expect(host.querySelector('[role="alert"]')?.textContent).toBe("upload interrupted")
    expect(field().readOnly).toBe(false)
    expect(host.querySelectorAll(".rw-files li")).toHaveLength(3)
    await submit()
    expect(send).toHaveBeenNthCalledWith(2, "first", "main", body, attachments, "queue")
    await show("first", "side")
    await act(async () => accept())
    expect(field().value).toBe("side draft")
    await show("first")
    expect(field().value).toBe("")
    expect(host.querySelector(".rw-files")).toBeNull()
  } finally {
    await act(async () => root.unmount())
    host.remove()
    HTMLElement.prototype.scrollIntoView = scrollIntoView
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
  }
})
