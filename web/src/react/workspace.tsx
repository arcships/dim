import { Button, Drawer, Modal, Popover } from "@heroui/react"
import { ArrowLeft, ChevronRight, CirclePlus, GitBranch, Menu, PanelLeftClose, X } from "lucide-react"
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type FormEvent, type ReactNode } from "react"
import { createPortal } from "react-dom"
import { appLocale, translate } from "./i18n"
import { useNewSessionModel } from "../features/chat/new-session-model"
import { ROOT_BRANCH_ID, type SessionSummary } from "../rpc/protocol"
import { useArchiveStore } from "../stores/archive"
import type { SessionView } from "../stores/chat"
import { useModels } from "../stores/models"
import { groupWorkspaces } from "../stores/sessions"
import { useUiStore } from "../stores/ui"
import { AccountSettings } from "./account-settings"
import { AgentSettings } from "./agent-settings"
import { AppearanceSettings } from "./appearance-settings"
import { ArchiveSettings } from "./archive-settings"
import { Composer, type ComposerHandle } from "./composer"
import { ModelPicker } from "./composer-controls"
import { DiagnosticsSettings } from "./diagnostics-settings"
import { DelegationsMenu, MemberConversation } from "./delegations"
import { ExtensionMenu, ExtensionPanel, useExtensionDirectory } from "./extensions"
import { McpSettings } from "./mcp-settings"
import { ModalitySettings } from "./modality-settings"
import { PermissionsSettings } from "./permissions-settings"
import { PluginSettings } from "./plugin-settings"
import { ProviderSettings } from "./provider-settings"
import { chat, connection, sessions, useStoreValue } from "./stores"
import { SessionDirectory, titleFor, type BranchDirectory } from "./sidebar"
import { Lines } from "../lines"
import { readBranches } from "../rpc/read"
import { SkillSettings } from "./skill-settings"
import { ScheduledSettings, type SchedulePage } from "./scheduled-settings"
import { Timeline } from "./timeline"

const archive = useArchiveStore()
const ui = useUiStore()
const readSessions = () => sessions.summaries.filter((session) => !archive.hidden.includes(session.sessionId)).map((session) => ({ ...session }))
const readPhase = () => connection.phase
const readAttachedBranches = () => new Map<string, BranchDirectory>(Object.values(chat.views).flatMap((view) =>
  view.status === "ready" ? [[view.sessionId, { lines: view.lines, defaultBranchId: view.defaultBranchId }]] : []))
const readLocale = () => appLocale.value
const readDirectoryError = () => sessions.error
const readDirectorySettled = () => sessions.loaded || sessions.error !== ""
const readSidebarCollapsed = () => ui.sidebarCollapsed
const readExtensionSelection = () => ui.detailOpen ? ui.extension : undefined
const readDetailWidth = () => ui.detailWidth

const settingsPages = [
  { id: "account", en: "Account", zh: "账户" },
  { id: "appearance", en: "Appearance", zh: "外观" },
  { id: "agent", en: "Agent", zh: "代理" },
  { id: "providers", en: "Providers", zh: "供应商" },
  { id: "modality", en: "Modality", zh: "多模态" },
  { id: "mcp", en: "MCP", zh: "MCP" },
  { id: "plugins", en: "Plugins", zh: "插件" },
  { id: "skills", en: "Skills", zh: "技能" },
  { id: "permissions", en: "Permissions", zh: "权限" },
  { id: "scheduled", en: "Scheduled tasks", zh: "定时任务" },
  { id: "archived", en: "Archived sessions", zh: "已归档的会话" },
  { id: "diagnostics", en: "Diagnostics", zh: "诊断" },
] as const
type SettingsPage = (typeof settingsPages)[number]["id"]
type WorkspaceRoute = { kind: "root" | "missing" } | { kind: "settings"; page: SettingsPage | "index"; detail?: string; schedule?: SchedulePage } | { kind: "new"; cwd?: string } | { kind: "session"; id: string; branchId?: string }

function currentRoute(): WorkspaceRoute {
  const pathname = location.pathname.replace(/\/+$/, "")
  if (pathname === "") return { kind: "root" }
  if (pathname === "/chat/new") return { kind: "new", cwd: new URLSearchParams(location.search).get("cwd") ?? undefined }
  const settingsPage = settingsPages.find(({ id }) => pathname === `/settings/${id}`)
  if (pathname === "/settings" || settingsPage) return { kind: "settings", page: settingsPage?.id ?? "index" }
  const settingsDetail = /^\/settings\/(skills|permissions|plugins|mcp|providers)\/([^/]+)$/.exec(pathname)
  const detailedPage = settingsPages.find(({ id }) => id === settingsDetail?.[1])
  if (settingsDetail && detailedPage) {
    try { return { kind: "settings", page: detailedPage.id, detail: decodeURIComponent(settingsDetail[2]!) } }
    catch { return { kind: "missing" } }
  }
  const scheduled = /^\/settings\/scheduled\/([^/]+)(\/edit)?$/.exec(pathname)
  if (scheduled) {
    try {
      const id = decodeURIComponent(scheduled[1]!)
      return { kind: "settings", page: "scheduled", detail: id,
        schedule: id === "new" && !scheduled[2] ? { kind: "new" } : { kind: "job", id, edit: scheduled[2] !== undefined } }
    } catch { return { kind: "missing" } }
  }
  const match = /^\/chat\/([^/]+)$/.exec(pathname)
  if (match === null) return { kind: "missing" }
  try { return { kind: "session", id: decodeURIComponent(match[1]!), branchId: new URLSearchParams(location.search).get("branch") || undefined } }
  catch { return { kind: "missing" } }
}

export function WorkspaceApp() {
  const directory = useStoreValue(readSessions)
  const directoryError = useStoreValue(readDirectoryError)
  const directorySettled = useStoreValue(readDirectorySettled)
  const phase = useStoreValue(readPhase)
  const locale = useStoreValue(readLocale)
  const text = (en: string, zh: string) => locale === "zh-CN" ? zh : en
  const workspaces = groupWorkspaces(directory)
  const members = new Map<string, SessionSummary[]>()
  for (const session of directory) {
    if (session.origin !== "internal" || session.parentSessionId === undefined) continue
    const group = members.get(session.parentSessionId)
    if (group) group.push(session)
    else members.set(session.parentSessionId, [session])
  }
  const [route, setRoute] = useState(currentRoute)
  const lastPath = useRef(location.pathname + location.search)
  const rememberedWorkspace = history.state?.workspaceReturn
  const settingsReturn = useRef<string>(typeof rememberedWorkspace === "string" && /^\/(?:chat\/[^?#]+(?:\?[^#]*)?)?$/.test(rememberedWorkspace)
    ? rememberedWorkspace : "/")
  const settingsHeading = useRef<HTMLElement>(null)
  const settingsOpen = route.kind === "settings"
  const settingsIndex = route.kind === "settings" && route.page === "index"
  const settingsPage = route.kind === "settings" ? (route.page === "index" ? "appearance" : route.page) : undefined
  const settingsDirty = useRef(false)
  const markSettingsDirty = useCallback((dirty: boolean) => { settingsDirty.current = dirty }, [])
  const confirmDiscard = useCallback(() => !settingsDirty.current || window.confirm(locale === "zh-CN"
    ? "放弃未保存的设置？" : "Discard unsaved settings?"), [locale])

  function navigate(path: string, replace: boolean) {
    if (path === lastPath.current) return
    const inSettings = path === "/settings" || path.startsWith("/settings/")
    if (inSettings && !settingsOpen) settingsReturn.current = lastPath.current
    const previousDepth = history.state?.settingsDepth
    const settingsDepth = settingsOpen
      ? typeof previousDepth === "number" && previousDepth > 0 ? previousDepth + (replace ? 0 : 1) : 0
      : replace ? 0 : 1
    const indexDistance = history.state?.settingsIndexDistance
    const settingsIndexDistance = path === "/settings" ? 0
      : settingsOpen && Number.isSafeInteger(indexDistance) ? indexDistance + (replace ? 0 : 1) : undefined
    const state = inSettings ? { workspaceReturn: settingsReturn.current, settingsDepth, settingsIndexDistance } : null
    if (replace) history.replaceState(state, "", path)
    else history.pushState(state, "", path)
    lastPath.current = path
    settingsDirty.current = false
  }
  const active = route.kind === "session" ? route.id : undefined
  const [tabs, setTabs] = useState<string[]>(() => active === undefined ? [] : [active])
  const [drawer, setDrawer] = useState(false)
  const [expandedSessions, setExpandedSessions] = useState<ReadonlySet<string>>(() => new Set())
  const [discoveredBranches, setDiscoveredBranches] = useState<ReadonlyMap<string, BranchDirectory>>(() => new Map())
  const [branchErrors, setBranchErrors] = useState<ReadonlyMap<string, string>>(() => new Map())
  const [newBranch, setNewBranch] = useState<{ sessionId: string; branchId: string; cut: number }>()
  const loadingBranches = useRef(new Set<string>())
  const attachedBranches = useStoreValue(readAttachedBranches)
  const branchDirectories = new Map([...discoveredBranches, ...attachedBranches])
  const activeBranch = route.kind === "session" ? route.branchId : undefined
  const [member, setMember] = useState<{ session: SessionSummary; trigger: HTMLButtonElement; fromDrawer: boolean }>()
  const collapsed = useStoreValue(readSidebarCollapsed)
  const [sidebarWidth, setSidebarWidth] = useState(250)
  const sidebarDrag = useRef<{ x: number; width: number } | undefined>(undefined)
  const [query, setQuery] = useState("")
  const [runningOnly, setRunningOnly] = useState(false)
  const [archiveError, setArchiveError] = useState("")

  function resizeSidebar(width: number) {
    setSidebarWidth(Math.min(400, Math.max(200, Math.round(width))))
  }

  useEffect(() => {
    function restore() {
      if (!confirmDiscard()) {
        history.pushState(null, "", lastPath.current)
        return
      }
      settingsDirty.current = false
      lastPath.current = location.pathname + location.search
      const next = currentRoute()
      if (next.kind === "settings") setDrawer(false)
      setRoute(next)
      if (next.kind === "session") setTabs((previous) => previous.includes(next.id) ? previous : [...previous, next.id])
    }
    function beforeUnload(event: BeforeUnloadEvent) {
      if (!settingsDirty.current) return
      event.preventDefault()
      event.returnValue = ""
    }
    window.addEventListener("popstate", restore)
    window.addEventListener("beforeunload", beforeUnload)
    return () => {
      window.removeEventListener("popstate", restore)
      window.removeEventListener("beforeunload", beforeUnload)
    }
  }, [confirmDiscard])

  useEffect(() => {
    if (phase === "connected" && active !== undefined) chat.open(active)
  }, [active, phase])

  useEffect(() => {
    if (route.kind !== "root" || !directorySettled) return
    const last = ui.lastSessionId
    const id = last !== undefined && sessions.byId.has(last) && !archive.has(last) ? last : undefined
    select(id, true)
  }, [route.kind, directorySettled])

  useEffect(() => {
    if (active !== undefined) ui.lastSessionId = active
  }, [active])

  useEffect(() => {
    if (active !== undefined && activeBranch !== undefined) {
      setExpandedSessions((previous) => previous.has(active) ? previous : new Set([...previous, active]))
    }
  }, [active, activeBranch])

  useEffect(() => {
    if (phase !== "connected") return
    for (const id of expandedSessions) void discoverBranches(id)
  }, [phase])

  useEffect(() => {
    function refreshDirectory() {
      if (connection.phase === "connected") void sessions.refresh()
    }
    window.addEventListener("focus", refreshDirectory)
    return () => window.removeEventListener("focus", refreshDirectory)
  }, [])

  useEffect(() => {
    const compact = matchMedia("(max-width: 719px)")
    const closeOnDesktop = (event: MediaQueryListEvent) => { if (!event.matches) setDrawer(false) }
    compact.addEventListener("change", closeOnDesktop)
    return () => compact.removeEventListener("change", closeOnDesktop)
  }, [])

  function select(id: string | undefined, replace = false, cwd?: string, branchId?: string) {
    const path = id === undefined ? `/chat/new${cwd === undefined ? "" : `?${new URLSearchParams({ cwd })}`}` : `/chat/${encodeURIComponent(id)}${branchId === undefined ? "" : `?${new URLSearchParams({ branch: branchId })}`}`
    if (!confirmDiscard()) return
    navigate(path, replace)
    if (id !== undefined) setTabs((previous) => previous.includes(id) ? previous : [...previous, id])
    setRoute(id === undefined ? { kind: "new", cwd } : { kind: "session", id, branchId })
    setDrawer(false)
  }

  function openBranch(sessionId: string, branchId: string) {
    setExpandedSessions((previous) => previous.has(sessionId) ? previous : new Set([...previous, sessionId]))
    select(sessionId, false, undefined, branchId)
  }

  async function discoverBranches(id: string) {
    if (loadingBranches.current.has(id) || attachedBranches.has(id)) return
    loadingBranches.current.add(id)
    setBranchErrors((previous) => {
      const next = new Map(previous)
      next.delete(id)
      return next
    })
    try {
      const { branches, defaultBranchId } = await connection.client.call("session.branches", { sessionId: id }, readBranches)
      const lines = new Lines()
      lines.directory(branches)
      setDiscoveredBranches((previous) => new Map(previous).set(id, { lines: lines.views(), defaultBranchId }))
    } catch (cause) {
      setBranchErrors((previous) => new Map(previous).set(id, cause instanceof Error ? cause.message : String(cause)))
    } finally {
      loadingBranches.current.delete(id)
    }
  }

  function close(id: string) {
    setTabs(tabs.filter((tab) => tab !== id))
    if (active === id) select(tabs[tabs.indexOf(id) + 1] ?? tabs[tabs.indexOf(id) - 1], true)
  }

  function archiveSession(id: string) {
    if (!archive.archive(id)) {
      setArchiveError("This browser cannot store the archive list.")
      return
    }
    setArchiveError("")
    close(id)
  }

  function openSettings(page: SettingsPage | "index") {
    if (route.kind !== "settings" || route.page !== page || route.detail !== undefined) {
      if (!confirmDiscard()) return
      navigate(page === "index" ? "/settings" : `/settings/${page}`, false)
      setRoute({ kind: "settings", page })
    }
    setDrawer(false)
  }

  function openSettingsPath(path: string, replace = false, saved = false) {
    if (!saved && !confirmDiscard()) return
    navigate(path, replace)
    setRoute(currentRoute())
  }

  function returnToWorkspace() {
    if (!confirmDiscard()) return
    const depth = history.state?.settingsDepth
    if (Number.isSafeInteger(depth) && depth > 0) {
      settingsDirty.current = false
      history.go(-depth)
      return
    }
    navigate(settingsReturn.current, true)
    const next = currentRoute()
    if (next.kind === "session") setTabs((previous) => previous.includes(next.id) ? previous : [...previous, next.id])
    setRoute(next)
  }

  function returnToSettingsIndex() {
    if (!confirmDiscard()) return
    const distance = history.state?.settingsIndexDistance
    if (Number.isSafeInteger(distance) && distance > 0) {
      settingsDirty.current = false
      history.go(-distance)
      return
    }
    navigate("/settings", true)
    setRoute({ kind: "settings", page: "index" })
  }

  useEffect(() => {
    if (settingsOpen) settingsHeading.current?.focus()
  }, [settingsOpen, settingsIndex])

  const selected = directory.find((session) => session.sessionId === active)
  useEffect(() => {
    document.title = settingsOpen ? `${text("Settings", "设置")} — Dim` : active === undefined ? "Dim" : `${titleFor(selected)} — Dim`
  }, [active, selected, settingsOpen, locale])

  if (phase === "unauthenticated" || phase === "version-mismatch") {
    return <main className="rw-entry-error"><h1>Dim</h1><p>{phase === "unauthenticated" ? "Open a fresh dimcode web link to connect." : "The App Server uses an incompatible protocol version."}</p></main>
  }

  const newSession = (cwd: string | undefined = selected?.cwd) => select(undefined, false, cwd)
  const directoryProps = {
    workspaces, members, expandedSessions, branchDirectories, branchErrors, active, activeBranch, phase, loaded: directorySettled, error: directoryError || archiveError,
    query, setQuery, runningOnly, setRunningOnly, select, newSession, archiveSession, openArchive: () => openSettings("archived"),
    openSettings: () => openSettings("index"),
    settingsLabel: text("Settings", "设置"),
    toggleSession: (id: string) => {
      const next = new Set(expandedSessions)
      if (!next.delete(id)) {
        next.add(id)
        if (phase === "connected") void discoverBranches(id)
      }
      setExpandedSessions(next)
    },
    retryBranches: (id: string) => { if (phase === "connected") void discoverBranches(id) },
    openBranch,
    createBranch: (sessionId: string, branchId: string) => {
      if (active !== sessionId || phase !== "connected" || !connection.supports("session.branch_create")) return undefined
      const session = chat.views[sessionId]
      const branch = branchId === session?.defaultBranchId ? session : session?.branches[branchId]
      const cut = branch?.lastTurnEnd
      if (cut === undefined || branch?.status !== "ready"
        || (branchId !== ROOT_BRANCH_ID && branchId !== session?.defaultBranchId)) return undefined
      return () => {
        setDrawer(false)
        setNewBranch({ sessionId, branchId, cut })
      }
    },
    openMember: (session: SessionSummary, trigger: HTMLButtonElement) => {
      setMember({ session, trigger, fromDrawer: drawer })
      setDrawer(false)
    },
  }
  return (
    <div className="rw-app" data-settings={settingsOpen} data-sidebar-collapsed={collapsed} style={{ "--rw-sidebar-width": `${sidebarWidth}px` } as CSSProperties}
      onKeyDown={(event) => {
        if (!settingsOpen && (event.metaKey || event.ctrlKey) && !event.shiftKey && !event.altKey && event.key.toLowerCase() === "b") {
          event.preventDefault()
          if (matchMedia("(max-width: 719px)").matches) setDrawer((open) => !open)
          else ui.sidebarCollapsed = !collapsed
        }
      }}>
      <aside className="rw-sidebar" aria-label="Workspace sessions" hidden={settingsOpen}>
        <div className="rw-sidebar-header">
          <strong className="rw-sidebar-title">Dim</strong>
          <button type="button" className="rw-sidebar-icon" aria-label="Hide workspace sidebar" onClick={() => { ui.sidebarCollapsed = true }}><PanelLeftClose size={16} aria-hidden="true" /></button>
        </div>
        <SessionDirectory {...directoryProps} />
        <div className="rw-sidebar-resize" role="separator" tabIndex={0} aria-label="Resize workspace sidebar" aria-orientation="vertical"
          aria-valuemin={200} aria-valuemax={400} aria-valuenow={sidebarWidth}
          onKeyDown={(event) => {
            if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return
            event.preventDefault()
            resizeSidebar(sidebarWidth + (event.key === "ArrowLeft" ? -16 : 16))
          }}
          onPointerDown={(event) => {
            if (event.button !== 0) return
            sidebarDrag.current = { x: event.clientX, width: sidebarWidth }
            event.currentTarget.setPointerCapture(event.pointerId)
          }}
          onPointerMove={(event) => {
            const drag = sidebarDrag.current
            if (drag !== undefined) resizeSidebar(drag.width + event.clientX - drag.x)
          }}
          onLostPointerCapture={() => { sidebarDrag.current = undefined }} />
      </aside>
      <main className="rw-main">
        <header className="rw-tabbar" hidden={settingsOpen}>
          <Drawer isOpen={drawer} onOpenChange={setDrawer}>
            <Button className="rw-mobile-menu" size="sm" variant="tertiary" isIconOnly aria-label="Show workspace sidebar"><Menu size={18} /></Button>
            <Drawer.Backdrop>
              <Drawer.Content placement="left" className="rw-sidebar-drawer">
                <Drawer.Dialog className="rw-sidebar-drawer-dialog" aria-label="Workspace sessions">
                  <header className="rw-sidebar-header">
                    <Drawer.Heading className="rw-sidebar-title">Dim</Drawer.Heading>
                    <button type="button" className="rw-sidebar-icon" aria-label="Close workspace sidebar" onClick={() => setDrawer(false)}><X size={16} aria-hidden="true" /></button>
                  </header>
                  <SessionDirectory {...directoryProps} />
                </Drawer.Dialog>
              </Drawer.Content>
            </Drawer.Backdrop>
          </Drawer>
          <Button className="rw-show-sidebar" size="sm" variant="tertiary" isIconOnly aria-label="Show workspace sidebar" onPress={() => { ui.sidebarCollapsed = false }}><Menu size={18} /></Button>
          <div className="rw-tabs" role="tablist" aria-label="Open sessions" onKeyDown={(event) => {
            const target = event.target
            if (!(target instanceof HTMLElement) || target.getAttribute("role") !== "tab") return
            const keys = ["ArrowLeft", "ArrowRight", "Home", "End"]
            if (!keys.includes(event.key)) return
            event.preventDefault()
            const index = tabs.indexOf(target.id.slice("session-tab-".length))
            const offset = event.key === "ArrowLeft" ? -1 : 1
            const next = tabs[event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1 : (index + offset + tabs.length) % tabs.length]
            if (next !== undefined) {
              select(next)
              document.getElementById(`session-tab-${next}`)?.focus()
            }
          }}>
            {tabs.map((id) => {
              const title = titleFor(directory.find((session) => session.sessionId === id))
              return <div className="rw-tab" key={id} data-active={active === id}>
                <button id={`session-tab-${id}`} type="button" role="tab" aria-controls={`session-pane-${id}`}
                  aria-selected={active === id} tabIndex={active === id || (active === undefined && id === tabs[0]) ? 0 : -1}
                  onClick={() => select(id)} title={title}>{title}</button>
                <button type="button" className="rw-tab-close" aria-label={`Close ${title}`} onClick={() => close(id)}><X size={12} aria-hidden="true" /></button>
              </div>
            })}
          </div>
          <Button className="rw-new-tab" size="sm" variant="tertiary" isIconOnly aria-label="New session" onPress={() => newSession()}><CirclePlus size={18} /></Button>
        </header>
        <div className="rw-panes">
          {route.kind === "new" && <NewSession key={route.cwd ?? ""} preferredWorkspace={route.cwd} workspaces={workspaces.map(({ cwd }) => cwd)} enabled={phase === "connected"} onCreated={select} />}
          {route.kind === "settings" && <div className="rw-settings-layout" data-index={settingsIndex}>
            <header className="rw-settings-header">
              <button type="button" className="rw-sidebar-action rw-settings-back-workspace" onClick={returnToWorkspace}>
                <ArrowLeft size={16} aria-hidden="true" /> {text("Back to workspace", "返回工作区")}
              </button>
              <button type="button" className="rw-sidebar-action rw-settings-back-index" onClick={returnToSettingsIndex}>
                <ArrowLeft size={16} aria-hidden="true" /> {text("Back to settings", "返回设置目录")}
              </button>
              <strong ref={settingsHeading} tabIndex={-1}>{text("Settings", "设置")}</strong>
            </header>
            <nav className="rw-settings-nav" aria-label={text("Settings", "设置")}>
              {settingsPages.map(({ id, en, zh }) => <a key={id} href={`/settings/${id}`}
                aria-current={route.page === id ? "page" : undefined} onClick={(event) => {
                  if (event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return
                  event.preventDefault()
                  openSettings(id)
                }}>{text(en, zh)}<ChevronRight size={15} aria-hidden="true" /></a>)}
            </nav>
            <div className="rw-settings-content">
              {settingsPage === "account" && <AccountSettings text={text} onDirty={markSettingsDirty} />}
              {settingsPage === "appearance" && <AppearanceSettings />}
              {settingsPage === "agent" && <AgentSettings text={text} onDirty={markSettingsDirty} />}
              {settingsPage === "providers" && <ProviderSettings detail={route.detail} text={text} onDirty={markSettingsDirty} open={openSettingsPath} />}
              {settingsPage === "modality" && <ModalitySettings text={text} onDirty={markSettingsDirty} />}
              {settingsPage === "mcp" && <McpSettings text={text} detail={route.detail} onDirty={markSettingsDirty} />}
              {settingsPage === "plugins" && <PluginSettings text={text} detail={route.detail} onDirty={markSettingsDirty} />}
              {settingsPage === "skills" && <SkillSettings text={text} detail={route.detail} />}
              {settingsPage === "permissions" && <PermissionsSettings text={text} onDirty={markSettingsDirty} />}
              {settingsPage === "scheduled" && <ScheduledSettings page={route.schedule ?? { kind: "list" }} text={text}
                onDirty={markSettingsDirty} openSession={select} open={openSettingsPath} />}
              {settingsPage === "archived" && <ArchiveSettings openSession={select} />}
              {settingsPage === "diagnostics" && <DiagnosticsSettings text={text} onDirty={markSettingsDirty} />}
            </div>
          </div>}
          {route.kind === "root" && <p className="rw-session-status" role="status">Loading conversations…</p>}
          {route.kind === "missing" && <section className="rw-new">
            <h1>Page not found</h1>
            <Button variant="secondary" onPress={() => select(undefined)}>Open a new session</Button>
          </section>}
          {tabs.map((id) => <SessionPane key={id} id={id} active={id === active} branch={id === active ? activeBranch : undefined} connected={phase === "connected"} openSession={select}
            openBranch={(branchId) => {
              const current = currentRoute()
              if (current.kind === "session" && current.id === id) openBranch(id, branchId)
            }}
            newSession={() => select(undefined, false, sessions.byId.get(id)?.cwd)}
            scheduled={() => {
              const cwd = sessions.byId.get(id)?.cwd
              openSettingsPath(`/settings/scheduled${cwd === undefined ? "" : `?${new URLSearchParams({ cwd })}`}`)
            }} />)}
        </div>
      </main>
      {newBranch && <ConversationActionDialog action={{ kind: "branch", cut: newBranch.cut, text: "" }}
        close={() => setNewBranch(undefined)} run={async (action, prompt) => {
          const id = await chat.createLine(newBranch.sessionId, newBranch.branchId, action.cut, prompt)
          openBranch(newBranch.sessionId, id)
          setNewBranch(undefined)
        }} />}
      {member && <MemberConversation sessionId={member.session.sessionId} title={titleFor(member.session)} close={() => {
        setMember(undefined)
        if (member.fromDrawer && matchMedia("(max-width: 719px)").matches) {
          setDrawer(true)
          requestAnimationFrame(() => document.querySelector<HTMLButtonElement>(`[role="dialog"] [data-member-id="${CSS.escape(member.session.sessionId)}"]`)?.focus())
        } else requestAnimationFrame(() => member.trigger.focus())
      }} />}
    </div>
  )
}

function ParentSessionLink({ id, title, open }: { id: string; title: string; open: (id: string) => void }) {
  const locale = useStoreValue(readLocale)
  const text = (en: string, zh: string) => locale === "zh-CN" ? zh : en
  return <a className="rw-parent-session" href={`/chat/${encodeURIComponent(id)}`} onClick={(event) => {
    if (event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return
    event.preventDefault()
    open(id)
  }}>{text(`Parent · ${title}`, `父会话 · ${title}`)}</a>
}

function SessionPane({ id, active, branch, connected, openSession, openBranch, newSession, scheduled }: {
  id: string
  active: boolean
  branch?: string
  connected: boolean
  openSession: (id: string) => void
  openBranch: (id: string) => void
  newSession: () => void
  scheduled: () => void
}) {
  const view = useStoreValue(useCallback(() => {
    const session = chat.views[id]
    return session === undefined ? undefined : { ...session }
  }, [id]))
  const parent = useStoreValue(useCallback(() => {
    const parentId = sessions.byId.get(id)?.parentSessionId
    const parentSession = parentId === undefined ? undefined : sessions.byId.get(parentId)
    return parentId === undefined ? undefined : { id: parentId, title: parentSession ? titleFor(parentSession) : parentId }
  }, [id]))
  const [centerSlot, setCenterSlot] = useState<HTMLDivElement | null>(null)
  const [visited, setVisited] = useState<ReadonlySet<string>>(new Set())
  const { directory, error: directoryError } = useExtensionDirectory()
  const selected = useStoreValue(readExtensionSelection)
  const width = useStoreValue(readDetailWidth)
  const extensionTrigger = useRef<HTMLButtonElement>(null)
  const showExtension = active && selected !== undefined
  const extension = directory?.extensions.find(({ pluginName }) => pluginName === selected?.pluginName)
  const panel = extension?.panels.find(({ id }) => id === selected?.panelId)
  const center = branch ?? view?.defaultBranchId ?? ROOT_BRANCH_ID
  const centerExists = center === ROOT_BRANCH_ID || center === view?.defaultBranchId || view?.lines.some(({ id }) => id === center)
  const centerName = center === ROOT_BRANCH_ID ? undefined : view?.lines.find(({ id }) => id === center)?.purpose || center
  const conversations = new Set([ROOT_BRANCH_ID, ...visited])
  if (centerExists) conversations.add(center)
  useEffect(() => {
    if (!active || !centerExists) return
    chat.ensureLine(id, center)
    setVisited((previous) => previous.has(center) ? previous : new Set([...previous, center]))
  }, [id, active, center, centerExists])

  const hasExtensions = directory?.extensions.some(({ panels }) => panels.length > 0) || directory?.errors.length || directoryError || showExtension
  const hasDelegations = view?.methods.includes("session.collaboration.pending") && view.methods.includes("session.collaboration.finished")
  return <div className="rw-session" id={`session-pane-${id}`} role="tabpanel" aria-labelledby={`session-tab-${id}`}
    data-active={active} data-extension={showExtension} inert={!active}
    style={showExtension ? { "--rw-extension-width": `${width}px` } as CSSProperties : undefined}>
    {view !== undefined && view.status !== "closed"
      ? <>
          <div className="rw-conversation">
            <header className="rw-session-heading">
              {centerName && <p className="rw-centered-line" title={centerName}><GitBranch size={15} /> <span>{centerName}</span></p>}
              {(parent || hasExtensions || hasDelegations) && <div className="rw-conversation-header rw-conversation-tools">
                {parent && <ParentSessionLink {...parent} open={openSession} />}
                <ExtensionMenu sessionId={id} directory={directory} directoryError={directoryError} connected={connected} active={active} trigger={extensionTrigger}
                  select={(value) => {
                    ui.extension = value
                    ui.detailOpen = true
                    ui.detailFocused = true
                  }} />
                {active && hasDelegations && <DelegationsMenu sessionId={id} revision={view.delegationRevision} />}
              </div>}
            </header>
            <div className="rw-conversation-slot" ref={setCenterSlot}>
              {!centerExists && view.status === "ready" && <p className="rw-branch-unavailable" role="status">
                This branch is unavailable. <button type="button" onClick={() => openSession(id)}>Return to conversation</button>
              </p>}
            </div>
          </div>
          {showExtension && <aside className="rw-workspace-extension" aria-label="Session extension">
            <ExtensionPanel sessionId={id} branchId={center}
              extension={extension} panel={panel} connected={connected} close={() => {
                ui.closeDetail()
                requestAnimationFrame(() => extensionTrigger.current?.focus())
              }} />
          </aside>}
          {[...conversations].map((branchId) => {
            const slot = branchId === center ? centerSlot : null
            return <ConversationSurface key={branchId} slot={slot}>
              <BranchConversation session={view} branchId={branchId} slot={slot} active={active && branchId === center}
                connected={connected} openLine={openBranch} openSession={openSession} newSession={newSession} scheduled={scheduled} />
            </ConversationSurface>
          })}
        </>
      : <p className="rw-session-status" role="status">{view?.status === "closed" ? "This session is closed." : "Loading conversation…"}</p>}
  </div>
}

function ConversationSurface({ slot, children }: { slot: HTMLDivElement | null; children: ReactNode }) {
  const [surface] = useState(() => document.createElement("div"))
  const scrollTop = useRef(0)
  useLayoutEffect(() => {
    const viewport = surface.querySelector<HTMLElement>(".rw-viewport")
    if (surface.isConnected && viewport && viewport.clientHeight > 0) scrollTop.current = viewport.scrollTop
    if (slot !== null) {
      slot.append(surface)
      if (viewport) viewport.scrollTop = scrollTop.current
    } else surface.remove()
  }, [slot, surface])
  useLayoutEffect(() => () => surface.remove(), [surface])
  return createPortal(children, surface)
}

type ConversationAction = { kind: "fork" | "branch"; cut: number; text: string }

function BranchConversation({ session, branchId, active, slot, connected, openLine, openSession, newSession, scheduled }: {
  session: SessionView
  branchId: string
  active: boolean
  slot: HTMLDivElement | null
  connected: boolean
  openLine: (id: string) => void
  openSession: (id: string) => void
  newSession: () => void
  scheduled: () => void
}) {
  const composer = useRef<ComposerHandle>(null)
  const [action, setAction] = useState<ConversationAction>()
  const isDefault = branchId === session.defaultBranchId
  const branch = isDefault ? session : session.branches[branchId]
  const state = session.lines.find((line) => line.id === branchId)?.state.kind
  const readOnly = state === "creation-failed" || state === "ending" || state?.startsWith("ended") === true
  const ready = connected && branch?.status === "ready"
  const pending = session.approvals.some((entry) => entry.branchId === branchId) || session.questions.some((entry) => entry.branchId === branchId)
  const canCompose = ready && !readOnly && !pending
  const canBranch = canCompose && (branchId === ROOT_BRANCH_ID || branchId === session.defaultBranchId) && connection.supports("session.branch_create")
  const forkCut = branch?.lastTurnEnd

  async function perform(selected: ConversationAction, prompt: string) {
    if (selected.kind === "branch") {
      openLine(await chat.createLine(session.sessionId, branchId, selected.cut, prompt))
    } else {
      const forked = await chat.fork(session.sessionId, selected.cut)
      void sessions.refresh()
      openSession(forked)
    }
    setAction(undefined)
  }

  return <><Timeline sessionId={session.sessionId} active={active} slot={slot} rows={branch?.rows ?? []} earlier={branch?.earlier ?? false}
    loadingEarlier={branch?.loadingEarlier ?? false} historyError={branch?.historyError ?? ""}
    actions={{
      quote: canCompose ? (text) => composer.current?.quote(text) : undefined,
      branch: canBranch
        ? (row) => setAction({ kind: "branch", cut: row.cut, text: row.text }) : undefined,
      fork: ready && branchId === ROOT_BRANCH_ID && connection.supports("session.fork") ? (row) => setAction({ kind: "fork", cut: row.cut, text: row.text }) : undefined,
    }}
    loadEarlier={() => chat.loadEarlier(session.sessionId, branchId)}>
    {(branch === undefined || branch.status === "loading") && <p role="status">Loading conversation…</p>}
    {branch?.status === "failed" && <>
      <p role="alert">{branch.error}</p>
      <Button size="sm" variant="tertiary" onPress={() => isDefault ? chat.retryCentre(session.sessionId) : chat.retryLine(session.sessionId, branchId)}>
        Retry loading line
      </Button>
    </>}
    <Composer ref={composer} session={session} branchId={branchId} connected={connected} readOnly={readOnly} actions={{
      newSession, scheduled,
      branch: canBranch ? async (words) => {
        if (branch?.lastTurnEnd === undefined) throw new Error(translate("lines.noFinishedTurn"))
        openLine(await chat.createLine(session.sessionId, branchId, branch.lastTurnEnd, words))
      } : undefined,
      fork: forkCut !== undefined && branchId === ROOT_BRANCH_ID && connection.supports("session.fork")
        ? () => setAction({ kind: "fork", cut: forkCut, text: "" })
        : undefined,
    }} />
  </Timeline>
  {active && action && <ConversationActionDialog action={action} close={() => setAction(undefined)} run={perform} />}
  </>
}

function ConversationActionDialog({ action, close, run }: {
  action: ConversationAction
  close: () => void
  run: (action: ConversationAction, prompt: string) => Promise<void>
}) {
  const [prompt, setPrompt] = useState(action.kind === "branch" ? action.text : "")
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const branch = action.kind === "branch"

  async function submit(event: FormEvent) {
    event.preventDefault()
    if (busy || (branch && !prompt.trim())) return
    setBusy(true)
    setError("")
    try { await run(action, prompt.trim()) }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false) }
  }

  return <Modal isOpen onOpenChange={(open) => { if (!open && !busy) close() }}>
    <Modal.Backdrop isDismissable={!busy}>
      <Modal.Container>
        <Modal.Dialog className="rw-conversation-action">
          <form onSubmit={(event) => { void submit(event) }}>
            <Modal.Header><Modal.Heading>{branch ? "Start a line" : "Fork session"}</Modal.Heading></Modal.Header>
            <Modal.Body>
              {!branch && action.text && <blockquote>{action.text}</blockquote>}
              {branch ? <label>Line prompt<textarea required value={prompt} onChange={(event) => setPrompt(event.target.value)} /></label>
                : <p>{action.text ? "Copy the root conversation up to the point before this message." : "Copy the root conversation through its last finished turn."}</p>}
              {error && <p className="rw-error" role="alert">{error}</p>}
            </Modal.Body>
            <Modal.Footer>
              <Button variant="tertiary" isDisabled={busy} onPress={close}>Cancel</Button>
              <Button type="submit" isPending={busy} isDisabled={branch && !prompt.trim()}>{branch ? "Start line" : "Fork"}</Button>
            </Modal.Footer>
          </form>
        </Modal.Dialog>
      </Modal.Container>
    </Modal.Backdrop>
  </Modal>
}

function NewSession({ workspaces, preferredWorkspace, enabled, onCreated }: {
  workspaces: string[]
  preferredWorkspace?: string
  enabled: boolean
  onCreated: (id: string) => void
}) {
  const [cwd, setCwd] = useState(preferredWorkspace ?? "")
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [modelChoice] = useState(() => useNewSessionModel(useModels().preview))
  const model = useStoreValue(useCallback(() => modelChoice.model, [modelChoice]))
  const resolution = useStoreValue(useCallback(() => modelChoice.resolution, [modelChoice]))
  const directory = cwd.trim() || workspaces[0] || ""

  useEffect(() => {
    if (!enabled || !connection.supports("session.model.preview")) return
    const timer = setTimeout(() => { void modelChoice.refresh(directory) }, 180)
    return () => clearTimeout(timer)
  }, [directory, enabled, modelChoice])

  async function create(event: FormEvent) {
    event.preventDefault()
    if (busy || !enabled || !directory) return
    setBusy(true)
    setError("")
    try { onCreated(await sessions.create(directory, modelChoice.override)) }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false) }
  }

  return <section className="rw-new">
    <h1>New session</h1>
    <form onSubmit={(event) => { void create(event) }}>
      <label htmlFor="rw-workspace">Workspace</label>
      <input id="rw-workspace" autoComplete="off" list="rw-workspaces" value={cwd} placeholder={workspaces[0] ?? "Absolute workspace path"} onChange={(event) => setCwd(event.target.value)} />
      <datalist id="rw-workspaces">{workspaces.map((path) => <option key={path} value={path} />)}</datalist>
      {connection.supports("provider.models.query") && <ModelPicker model={model} disabled={!enabled || busy}
        choose={modelChoice.choose} follow={modelChoice.follow} />}
      {resolution.state === "failed" && <p className="rw-muted">{resolution.message}</p>}
      {error && <p className="rw-error" role="alert">{error}</p>}
      <Button type="submit" variant="primary" isDisabled={!enabled || !directory} isPending={busy}>Create session</Button>
    </form>
  </section>
}
