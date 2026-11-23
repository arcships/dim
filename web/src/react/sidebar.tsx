import { Button, Popover } from "@heroui/react"
import { Archive, ArrowDownUp, ChevronRight, Folder, GitBranch, GitFork, Monitor, Plus, Search, Settings } from "lucide-react"
import { useCallback, useId, useState } from "react"
import { sortKey } from "../components/sidebar-sort"
import { ROOT_BRANCH_ID, type SessionSummary } from "../rpc/protocol"
import type { LineView } from "../lines"
import { BranchActions, branchStates } from "./branch-actions"
import type { ConnectionPhase } from "../rpc/client"
import type { Workspace } from "../stores/sessions"
import { useUiStore } from "../stores/ui"
import { useStoreValue } from "./stores"

const ui = useUiStore()
const readSort = () => ui.sidebarSortBy

export function titleFor(session: SessionSummary | undefined): string {
  return session?.title ?? session?.preview ?? "Untitled session"
}

export type BranchDirectory = { lines: readonly LineView[]; defaultBranchId: string }

type DirectoryBranches = {
  branchDirectories: ReadonlyMap<string, BranchDirectory>
  branchErrors: ReadonlyMap<string, string>
  expandedSessions: ReadonlySet<string>
  toggleSession: (id: string) => void
  retryBranches: (id: string) => void
  openBranch: (sessionId: string, branchId: string) => void
  createBranch: (sessionId: string, branchId: string) => (() => void) | undefined
  activeBranch: string | undefined
}

type SessionDirectoryProps = DirectoryBranches & {
  workspaces: Workspace[]
  members: ReadonlyMap<string, SessionSummary[]>
  openMember: (member: SessionSummary) => void
  active: string | undefined
  phase: ConnectionPhase
  loaded: boolean
  error: string
  query: string
  setQuery: (query: string) => void
  runningOnly: boolean
  setRunningOnly: (runningOnly: boolean) => void
  select: (id: string) => void
  newSession: (cwd?: string) => void
  archiveSession: (id: string) => void
  openArchive: () => void
  openSettings: () => void
  settingsLabel: string
}

type DirectoryRow = { session: SessionSummary; members: SessionSummary[]; directory?: BranchDirectory; branches: LineView[] }

export function SessionDirectory({ workspaces, members, openMember, active, phase, loaded, error, query, setQuery, runningOnly, setRunningOnly, select, newSession, archiveSession, openArchive, openSettings, settingsLabel, ...branchProps }: SessionDirectoryProps) {
  const sort = useStoreValue(readSort)
  const search = query.trim().toLocaleLowerCase()
  const includes = (text: string | undefined) => text?.toLocaleLowerCase().includes(search) ?? false
  const bySort = (left: SessionSummary, right: SessionSummary) => sortKey(right, sort) - sortKey(left, sort)
  const visible = workspaces.flatMap((workspace) => {
    const matchWorkspace = search === "" || includes(workspace.name) || includes(workspace.cwd)
    const matches = (session: SessionSummary) => matchWorkspace || includes(titleFor(session)) || includes(session.preview)
    const rows = [...workspace.sessions].sort(bySort).flatMap((session) => {
      const matchParent = matches(session)
      const children = (members.get(session.sessionId) ?? []).filter((member) => (!runningOnly || member.running) && (matchParent || matches(member))).sort(bySort)
      const directory = branchProps.branchDirectories.get(session.sessionId)
      const lines = directory?.lines.filter((line) => line.id !== ROOT_BRANCH_ID) ?? []
      const visibleIds = new Set(lines.filter((line) => (!runningOnly || line.state.kind === "working")
        && (matchParent || includes(line.purpose || line.id))).map((line) => line.id))
      for (const line of [...lines].reverse()) {
        if (line.parentId !== null && visibleIds.has(line.id)) visibleIds.add(line.parentId)
      }
      const branches = lines.filter(({ id }) => visibleIds.has(id))
      return children.length > 0 || branches.length > 0 || (matchParent && (!runningOnly || session.running))
        ? [{ session, members: children, directory, branches }] : []
    })
    return rows.length ? [{ workspace, rows }] : []
  }).sort((left, right) => bySort(left.rows[0]!.session, right.rows[0]!.session))

  return <>
    <button type="button" className="rw-sidebar-action rw-new-button" onClick={() => newSession()}><Plus size={16} aria-hidden="true" /> New session</button>
    <div className="rw-directory-controls">
      <label className="rw-directory-search">
        <Search size={15} aria-hidden="true" />
        <input type="search" aria-label="Search sessions" placeholder="Search sessions" value={query} onChange={(event) => setQuery(event.target.value)} />
      </label>
      <Popover>
        <Button className="rw-sidebar-icon" variant="tertiary" isIconOnly aria-label="Session sort and filters"><ArrowDownUp size={15} aria-hidden="true" /></Button>
        <Popover.Content placement="bottom end" className="rw-directory-filter">
          <Popover.Dialog aria-label="Session sort and filters">
            <fieldset>
              <legend>Sort by</legend>
              <label><input type="radio" name="workspace-sort" checked={sort === "recentActive"} onChange={() => { ui.sidebarSortBy = "recentActive" }} /> Recent activity</label>
              <label><input type="radio" name="workspace-sort" checked={sort === "recentCreated"} onChange={() => { ui.sidebarSortBy = "recentCreated" }} /> Recently created</label>
            </fieldset>
            <label><input type="checkbox" checked={runningOnly} onChange={(event) => setRunningOnly(event.target.checked)} /> Running only</label>
          </Popover.Dialog>
        </Popover.Content>
      </Popover>
    </div>
    <div className="rw-workspaces">
      {phase !== "connected" && <p className="rw-muted" role="status">{phase === "reconnecting" ? "Reconnecting…" : "Connecting…"}</p>}
      {error && <p className="rw-error" role="alert">{error}</p>}
      {loaded && visible.length === 0 && <p className="rw-directory-empty">{search || runningOnly ? "No matching sessions" : "No sessions yet"}</p>}
      {visible.map(({ workspace, rows }) => <SessionGroup key={workspace.cwd} workspace={workspace} rows={rows} active={active}
        searching={search !== ""} phase={phase} {...branchProps} openMember={openMember}
        select={select} newSession={newSession} archiveSession={archiveSession} />)}
    </div>
    <footer className="rw-directory-footer">
      <button type="button" className="rw-sidebar-action" onClick={openArchive}>
        <Archive size={15} aria-hidden="true" /> Archived sessions
      </button>
      <button type="button" className="rw-sidebar-action" onClick={openSettings}>
        <Settings size={15} aria-hidden="true" /> {settingsLabel}
      </button>
    </footer>
  </>
}

const initialRows = 5
const additionalRows = 20

function SessionGroup({ workspace, rows, active, searching, phase, expandedSessions, toggleSession, retryBranches, activeBranch, openBranch, createBranch, branchErrors, openMember, select, newSession, archiveSession }: DirectoryBranches & {
  workspace: Workspace
  rows: DirectoryRow[]
  active: string | undefined
  searching: boolean
  phase: ConnectionPhase
  openMember: SessionDirectoryProps["openMember"]
  select: (id: string) => void
  newSession: (cwd: string) => void
  archiveSession: (id: string) => void
}) {
  const expanded = useStoreValue(useCallback(() => ui.isWorkspaceExpanded(workspace.cwd), [workspace.cwd]))
  const [limit, setLimit] = useState(initialRows)
  const listId = useId()
  const open = searching || expanded
  const visible = searching ? rows : rows.slice(0, limit)
  const hasMore = visible.length < rows.length
  const activeWork = !open && rows.some(({ session, members, branches }) => session.running || members.some((member) => member.running) || branches.some((line) => line.state.kind === "working"))
  return <section className="rw-directory-group" aria-label={workspace.name}>
    <div className="rw-group-heading">
      <button className="rw-group-toggle" type="button" aria-expanded={open} aria-controls={listId} aria-disabled={searching || undefined} title={workspace.cwd}
        onClick={() => { if (!searching) ui.toggleWorkspace(workspace.cwd) }}>
        <ChevronRight className="rw-group-chevron" size={14} aria-hidden="true" />
        <Folder size={14} aria-hidden="true" />
        <span>{workspace.name}</span>
        {activeWork && <span className="rw-live" role="img" aria-label="Running session" />}
      </button>
      <button type="button" className="rw-sidebar-icon" aria-label={`New session in ${workspace.name}`}
        onClick={() => newSession(workspace.cwd)}><Plus size={15} aria-hidden="true" /></button>
      {!open && <button type="button" className="rw-group-count" aria-label={`Expand ${workspace.name}, ${rows.length} sessions`}
        aria-controls={listId} aria-expanded={false} onClick={() => ui.toggleWorkspace(workspace.cwd)}>{rows.length}</button>}
    </div>
    <div id={listId} className="rw-session-slide" data-open={open} inert={!open}>
      <div className="rw-session-clip">
        <ul>
        {visible.map(({ session, members, directory, branches }) => {
          const title = titleFor(session)
          const preview = session.preview === title ? undefined : session.preview
          const sessionId = session.sessionId
          const childrenOpen = searching || expandedSessions.has(sessionId)
          const canExpand = branches.length > 0 || members.length > 0 || (!searching && directory === undefined)
          const discoveryError = directory === undefined ? branchErrors.get(sessionId) : undefined
          const root = directory?.lines.find(({ id }) => id === ROOT_BRANCH_ID)
          const rootMenu = root && directory && <BranchActions sessionId={sessionId} line={root} lines={directory.lines}
            defaultBranchId={directory.defaultBranchId} connected={phase === "connected"} useDefault={() => select(sessionId)}
            create={createBranch(sessionId, directory.defaultBranchId)} />
          return <li key={session.sessionId}>
            <div className="rw-directory-row-main">
              <button className="rw-directory-session" type="button" aria-label={title} aria-description={session.running ? "Running" : undefined}
                aria-current={active === sessionId && activeBranch === undefined ? "page" : undefined} onClick={() => select(sessionId)}>
                <Monitor className="rw-session-icon" size={15} aria-hidden="true" />
                <span className="rw-session-copy"><span>{title}</span>{preview && <small>{preview}</small>}</span>
                {session.running && <span className="rw-live" aria-hidden="true" />}
              </button>
              {(canExpand || !session.running || rootMenu) && <div className="rw-directory-row-actions">
                {rootMenu}
                {!session.running && <button type="button" className="rw-sidebar-icon rw-directory-row-action"
                  aria-label={`Archive ${title}`} onClick={() => archiveSession(session.sessionId)}><Archive size={14} aria-hidden="true" /></button>}
                {canExpand && <button type="button" className="rw-sidebar-icon rw-member-toggle"
                  aria-label={`Branches and members for ${title}`} aria-expanded={childrenOpen} disabled={searching || (phase !== "connected" && !directory && !members.length)}
                  aria-controls={`${listId}-children-${sessionId}`} onClick={() => toggleSession(sessionId)}>
                  <ChevronRight size={14} aria-hidden="true" />
                </button>}
              </div>}
            </div>
            {canExpand && <ul className="rw-member-list" id={`${listId}-children-${sessionId}`} hidden={!childrenOpen}>
              {directory && branches.map((line) => {
                const name = line.purpose || line.id
                return <li key={line.id} className="rw-branch-row" style={{ marginInlineStart: `${Math.min(line.depth - 1, 3) * .65}rem` }}>
                  <button type="button" className="rw-directory-session rw-directory-member" aria-label={name}
                    aria-current={active === sessionId && activeBranch === line.id ? "page" : undefined}
                    disabled={line.state.kind === "creation-failed"} onClick={() => openBranch(sessionId, line.id)}>
                    <GitBranch size={14} aria-hidden="true" />
                    <span className="rw-session-copy"><span>{name}</span><small>{line.id === directory.defaultBranchId ? "Current conversation" : branchStates[line.state.kind]}</small></span>
                    {line.state.kind === "working" && <span className="rw-live" aria-hidden="true" />}
                  </button>
                  <BranchActions sessionId={sessionId} line={line} lines={directory.lines} defaultBranchId={directory.defaultBranchId}
                    connected={phase === "connected"} useDefault={() => select(sessionId)} create={createBranch(sessionId, line.id)} />
                </li>
              })}
              {!searching && directory === undefined && <li className="rw-branch-discovery">
                {discoveryError ? <><small role="alert">{discoveryError}</small><button type="button" onClick={() => retryBranches(sessionId)}>Retry</button></>
                  : <small role="status">{phase === "connected" ? "Loading branches…" : "Waiting for connection…"}</small>}
              </li>}
              {members.map((member) => {
                const memberTitle = titleFor(member)
                return <li key={member.sessionId}>
                  <button type="button" className="rw-directory-session rw-directory-member rw-native-member" aria-current={active === member.sessionId ? "page" : undefined}
                    aria-label={memberTitle} aria-description={member.running ? "Running" : undefined} onClick={() => openMember(member)}>
                    <GitFork size={14} aria-hidden="true" />
                    <span className="rw-session-copy"><span>{memberTitle}</span>{member.preview && member.preview !== memberTitle && <small>{member.preview}</small>}</span>
                    {member.running && <span className="rw-live" aria-hidden="true" />}
                  </button>
                </li>
              })}
            </ul>}
          </li>
        })}
        {!searching && rows.length > initialRows && <li>
          <button type="button" className="rw-sidebar-action rw-directory-more"
            onClick={() => setLimit(hasMore ? limit + additionalRows : initialRows)}>{hasMore ? "Show older sessions" : "Show fewer sessions"}</button>
        </li>}
        </ul>
      </div>
    </div>
  </section>
}
