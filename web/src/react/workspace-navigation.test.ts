import React, { act, createElement } from "react"
import { createRoot } from "react-dom/client"
import { expect, test, vi } from "vitest"
import { WorkspaceApp } from "./workspace"
import { chat, connection, sessions } from "./stores"

vi.hoisted(() => vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {}, removeEventListener() {} })))
vi.mock("#workspace-store", () => import("./define-store"))
vi.mock("#workspace-i18n", () => import("./i18n"))

test("only the original create form may open the session when its reply arrives", async () => {
  vi.stubGlobal("React", React)
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true)
  const path = "/chat/new?cwd=%2Fwork"
  history.replaceState(null, "", path)
  connection.phase = "connected"
  vi.spyOn(chat, "open").mockImplementation(() => {})
  let created!: (id: string) => void
  const create = vi.spyOn(sessions, "create").mockImplementation(() => new Promise(resolve => { created = resolve }))
  const host = document.body.appendChild(document.createElement("div"))
  const root = createRoot(host)
  const submit = () => act(async () => host.querySelector("form")!.requestSubmit())
  const navigate = (path: string) => act(async () => {
    history.pushState(null, "", path)
    window.dispatchEvent(new PopStateEvent("popstate"))
  })
  try {
    await act(async () => root.render(createElement(WorkspaceApp)))
    await submit()
    await navigate("/chat/other")
    await navigate(path)
    await act(async () => created("background"))
    expect(location.pathname + location.search).toBe(path)
    await submit()
    await act(async () => created("foreground"))
    expect(location.pathname).toBe("/chat/foreground")
    expect(create).toHaveBeenCalledTimes(2)
  } finally {
    await act(async () => root.unmount())
    host.remove()
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
  }
})
