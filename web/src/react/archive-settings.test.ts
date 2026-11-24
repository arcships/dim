import React, { act, createElement } from "react"
import { createRoot } from "react-dom/client"
import { expect, test, vi } from "vitest"
import { RpcError } from "../rpc/client"
import { useArchiveStore } from "../stores/archive"
import { ArchiveSettings } from "./archive-settings"
import { setAppLocale } from "./i18n"
import { connection } from "./stores"

vi.hoisted(() => vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {}, removeEventListener() {} })))
vi.mock("#workspace-store", () => import("./define-store"))
vi.mock("#workspace-i18n", () => import("./i18n"))

test("filtered archive deletion confirms and deletes only visible, inactive candidates", async () => {
  vi.stubGlobal("React", React)
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true)
  const summaries = [
    ["first", "Match A", "/one"], ["race", "Match B", "/one"], ["last", "Match C", "/one"],
    ["working", "Match running", "/one"], ["other", "Match elsewhere", "/two"],
    ["different", "Different", "/one"], ["ordinary", "Match not archived", "/one"],
  ].map(([sessionId, preview, cwd]) => ({ sessionId, preview, cwd, running: sessionId === "working",
    model: { modelId: "model", providerId: "fixture" }, title: null, titleSource: null, titleRevision: 0,
    createdAt: 1, updatedAt: 2, sequence: 1 }))
  const archive = useArchiveStore()
  for (const id of ["first", "race", "last", "working", "different", "other", "missing"]) archive.archive(id)
  connection.phase = "connected"
  vi.spyOn(connection, "supports").mockImplementation(method => method === "session.delete")
  const deletes: Record<string, unknown>[] = []
  vi.spyOn(connection.client, "call").mockImplementation(async (method, params, read) => {
    if (method === "session.summaries") return read(summaries)
    expect(method).toBe("session.delete")
    deletes.push(params)
    if (params.sessionId === "race") throw new RpcError(-32000, "in use", "session_active")
    return read({ deleted: true })
  })
  const confirm = vi.spyOn(window, "confirm").mockReturnValue(false)
  const container = document.body.appendChild(document.createElement("div"))
  const root = createRoot(container)
  const titles = () => [...container.querySelectorAll("li strong")].map(el => el.textContent)
  const bulk = () => [...container.querySelectorAll("button")].find(el => el.textContent.includes("匹配的已归档会话"))
  async function search(query: string) {
    const input = container.querySelector<HTMLInputElement>('input[type="search"]')!
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, query)
      input.dispatchEvent(new Event("input", { bubbles: true }))
    })
  }
  try {
    await act(async () => root.render(createElement(ArchiveSettings, { openSession: vi.fn() })))
    expect(container.querySelector('input[type="search"]')?.getAttribute("aria-label")).toBe("Search archived sessions")
    expect(container.querySelector('[aria-label="Delete Match running"]')?.hasAttribute("disabled")).toBe(true)
    await act(async () => setAppLocale("zh-CN"))
    expect(container.querySelector('input[type="search"]')?.getAttribute("aria-label")).toBe("搜索已归档会话")
    await search("mATch")
    expect(titles()).toEqual(["Match A", "Match B", "Match C", "Match running", "Match elsewhere"])
    await act(async () => {
      const select = container.querySelector("select")!
      select.value = [...select.options].find(option => option.textContent.includes("/one"))!.value
      select.dispatchEvent(new Event("change", { bubbles: true }))
    })
    expect(titles()).toEqual(["Match A", "Match B", "Match C", "Match running"])
    await search("no matches")
    expect(titles()).toEqual([])
    expect(bulk()).toBeUndefined()
    expect(container.textContent).toContain("没有匹配的会话")
    await search("match")
    await act(async () => bulk()!.click())
    expect(confirm).toHaveBeenCalledWith("永久删除 3 个匹配的已归档会话（“Match A”、“Match B”、“Match C”）及其历史？此操作不可撤销。")
    expect(deletes).toEqual([])
    confirm.mockReturnValue(true)
    await act(async () => bulk()!.click())
    expect(deletes).toEqual(["first", "race", "last"].map(sessionId => ({ sessionId, requireInactive: true })))
    expect(titles()).toEqual(["Match B", "Match running"])
    expect(container.textContent).toContain("已删除 2/3 个已归档会话")
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("此会话正在使用中，未删除。")
    expect(archive.hidden).toEqual(["race", "working", "different", "other", "missing"])
  } finally {
    await act(async () => root.unmount())
    container.remove()
    localStorage.clear()
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
  }
})
