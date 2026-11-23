import { FieldSelect } from "./field-select"
import { Button } from "@heroui/react"
import { ArchiveRestore, RotateCw, Trash2 } from "lucide-react"
import { useCallback, useEffect, useRef, useState } from "react"
import { RpcError } from "../rpc/client"
import type { SessionSummary } from "../rpc/protocol"
import { readAck, readSummaries } from "../rpc/read"
import { useArchiveStore } from "../stores/archive"
import { workspaceName } from "../stores/sessions"
import { titleFor } from "./sidebar"
import { connection, sessions, useStoreValue } from "./stores"

const archive = useArchiveStore()
const readHidden = () => archive.hidden
const readConnected = () => connection.phase === "connected"

export function ArchiveSettings({ openSession }: { openSession: (id: string) => void }) {
  const hidden = useStoreValue(readHidden)
  const connected = useStoreValue(readConnected)
  const [rows, setRows] = useState<SessionSummary[]>([])
  const [listingValid, setListingValid] = useState(false)
  const [busy, setBusy] = useState(false)
  const working = useRef(false)
  const [query, setQuery] = useState("")
  const [cwd, setCwd] = useState("")
  const [error, setError] = useState("")
  const [rowErrors, setRowErrors] = useState<Record<string, string>>({})
  const [report, setReport] = useState("")

  const archived = rows.filter((row) => hidden.includes(row.sessionId))
  const workspaces = [...new Set(archived.map((row) => row.cwd))]
  const workspace = workspaces.includes(cwd) ? cwd : ""
  const search = query.trim().toLocaleLowerCase()
  const visible = archived.filter((row) => (workspace === "" || row.cwd === workspace)
    && [row.title, row.preview, row.cwd].some((text) => text?.toLocaleLowerCase().includes(search)))
  const missing = listingValid ? hidden.filter((id) => !rows.some((row) => row.sessionId === id)) : []
  const canDelete = connected && connection.supports("session.delete")

  const refresh = useCallback(async () => {
    if (working.current) return
    working.current = true
    setBusy(true)
    setError("")
    setListingValid(false)
    try {
      let result: SessionSummary[]
      try {
        result = await connection.client.call("session.summaries", { includeClosed: true }, readSummaries)
      } catch (cause) {
        if (!(cause instanceof RpcError) || cause.appError !== "invalid_params") throw cause
        result = await connection.client.call("session.summaries", {}, readSummaries)
      }
      setRows(result)
      setListingValid(true)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      working.current = false
      setBusy(false)
    }
  }, [])

  useEffect(() => { if (connected) void refresh() }, [connected, refresh])

  function restore(id: string, open = false) {
    if (!archive.restore(id)) {
      setError("This browser cannot store the archive list.")
      return
    }
    setError("")
    if (open) openSession(id)
  }

  async function deleteRows(targets: readonly SessionSummary[]) {
    if (working.current || !canDelete || targets.length === 0) return
    const target = targets.length === 1 ? `“${titleFor(targets[0])}”` : `all ${targets.length} archived sessions`
    if (!window.confirm(`Permanently delete ${target} and its history? This cannot be undone.`)) return
    working.current = true
    setBusy(true)
    setError("")
    setRowErrors({})
    setReport("")
    const deleted = new Set<string>()
    const failures: Record<string, string> = {}
    for (const row of targets) {
      try {
        await connection.client.call("session.delete", { sessionId: row.sessionId, requireInactive: true }, readAck)
        if (!archive.restore(row.sessionId)) setError("This browser cannot store the archive list.")
        sessions.forget(row.sessionId)
        deleted.add(row.sessionId)
      } catch (cause) {
        failures[row.sessionId] = cause instanceof RpcError && cause.appError === "session_active"
          ? "This session is in use and was not deleted."
          : cause instanceof Error ? cause.message : String(cause)
      }
    }
    setRows((previous) => previous.filter((row) => !deleted.has(row.sessionId)))
    setRowErrors(failures)
    setReport(`Deleted ${deleted.size} of ${targets.length} archived sessions.`)
    working.current = false
    setBusy(false)
  }

  return <section className="rw-settings rw-archive">
    <h1>Archived sessions</h1>
    <div className="rw-field-row">
      <input type="search" aria-label="Search archived sessions" placeholder="Search archived sessions" value={query} onChange={(event) => setQuery(event.target.value)} />
      <FieldSelect label={"Filter by workspace"} value={workspace}
        onChange={(selectedValue) => setCwd(selectedValue)} options={[
          { value: "", label: "All workspaces" },
          ...workspaces.map((path) => ({ value: path, label: workspaceName(path) })),
        ]} />
      <Button size="sm" variant="secondary" isDisabled={!connected || busy} onPress={() => { void refresh() }}><RotateCw size={15} /> Refresh</Button>
    </div>
    {error && <p className="rw-error" role="alert">{error}</p>}
    {report && <p role="status">{report}</p>}
    {!connected && <p className="rw-muted" role="status">The App Server is disconnected.</p>}
    {busy && <p className="rw-muted" role="status">Working…</p>}
    {listingValid && visible.length === 0 && <p className="rw-muted">{archived.length === 0 ? "No archived sessions" : "No matching sessions"}</p>}
    <ul>
      {visible.map((row) => <li key={row.sessionId} className="rw-archive-row">
        <div>
          <strong>{titleFor(row)}</strong><small>{row.cwd}</small>
          {rowErrors[row.sessionId] && <p className="rw-error" role="alert">{rowErrors[row.sessionId]}</p>}
        </div>
        <div className="rw-setting-options">
          {!row.closed && <Button size="sm" variant="tertiary" isDisabled={busy} onPress={() => restore(row.sessionId, true)}>Restore and open</Button>}
          <Button size="sm" variant="secondary" aria-label={`Restore ${titleFor(row)}`} isDisabled={busy} onPress={() => restore(row.sessionId)}><ArchiveRestore size={15} /> Restore</Button>
          <Button size="sm" variant="danger-soft" aria-label={`Delete ${titleFor(row)}`} isIconOnly isDisabled={busy || !canDelete}
            onPress={() => { void deleteRows([row]) }}><Trash2 size={15} /></Button>
        </div>
      </li>)}
    </ul>
    {missing.length > 0 && <p>{missing.length} unavailable archive markers. <Button size="sm" variant="tertiary" isDisabled={busy}
      onPress={() => { if (!archive.drop(missing)) setError("This browser cannot store the archive list.") }}>Remove markers</Button></p>}
    {archived.length > 1 && <Button size="sm" variant="danger-soft" isDisabled={busy || !canDelete}
      onPress={() => { void deleteRows(archived) }}>Delete all archived sessions</Button>}
  </section>
}
