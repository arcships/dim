import { FieldSelect } from "./field-select"
import { Button } from "@heroui/react"
import { ArchiveRestore, RotateCw, Trash2 } from "lucide-react"
import { useCallback, useEffect, useRef, useState } from "react"
import { RpcError } from "../rpc/client"
import type { SessionSummary } from "../rpc/protocol"
import { readAck, readSummaries } from "../rpc/read"
import { useArchiveStore } from "../stores/archive"
import { workspaceName } from "../stores/sessions"
import { appLocale } from "./i18n"
import { titleFor } from "./sidebar"
import { connection, sessions, useStoreValue } from "./stores"

const archive = useArchiveStore()
const readHidden = () => archive.hidden
const readConnected = () => connection.phase === "connected"
const readLocale = () => appLocale.value

export function ArchiveSettings({ openSession }: { openSession: (id: string) => void }) {
  const hidden = useStoreValue(readHidden)
  const connected = useStoreValue(readConnected)
  const locale = useStoreValue(readLocale)
  const text = (en: string, zh: string) => locale === "zh-CN" ? zh : en
  const storageError = text("This browser cannot store the archive list.", "此浏览器无法保存归档列表。")
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
  const workspaces = new Set(archived.map((row) => row.cwd))
  if (cwd) workspaces.add(cwd)
  const search = query.trim().toLocaleLowerCase()
  const visible = archived.filter((row) => (cwd === "" || row.cwd === cwd)
    && [row.title, row.preview, row.cwd].some((text) => text?.toLocaleLowerCase().includes(search)))
  const deletable = visible.filter((row) => !row.running)
  const missing = listingValid ? hidden.filter((id) => !rows.some((row) => row.sessionId === id)) : []
  const canDelete = listingValid && connected && connection.supports("session.delete")

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
      setError(storageError)
      return
    }
    setError("")
    if (open) openSession(id)
  }

  async function deleteRows(targets: readonly SessionSummary[]) {
    if (working.current || !canDelete || targets.length === 0) return
    const names = targets.slice(0, 3).map((row) => `“${row.preview ?? row.title ?? row.sessionId}”`)
    if (targets.length > names.length) names.push("…")
    const confirmation = targets.length === 1
      ? text(`Permanently delete ${names[0]} and its history? This cannot be undone.`, `永久删除${names[0]}及其历史？此操作不可撤销。`)
      : text(`Permanently delete ${targets.length} matching archived sessions (${names.join(", ")}) and their history? This cannot be undone.`,
        `永久删除 ${targets.length} 个匹配的已归档会话（${names.join("、")}）及其历史？此操作不可撤销。`)
    if (!window.confirm(confirmation)) return
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
        if (!archive.restore(row.sessionId)) setError(storageError)
        sessions.forget(row.sessionId)
        deleted.add(row.sessionId)
      } catch (cause) {
        failures[row.sessionId] = cause instanceof RpcError && cause.appError === "session_active"
          ? text("This session is in use and was not deleted.", "此会话正在使用中，未删除。")
          : cause instanceof Error ? cause.message : String(cause)
      }
    }
    setRows((previous) => previous.filter((row) => !deleted.has(row.sessionId)))
    setRowErrors(failures)
    setReport(text(`Deleted ${deleted.size} of ${targets.length} archived sessions.`, `已删除 ${deleted.size}/${targets.length} 个已归档会话。`))
    working.current = false
    setBusy(false)
  }

  return <section className="rw-settings rw-archive">
    <h1>{text("Archived sessions", "已归档会话")}</h1>
    <div className="rw-field-row">
      <input type="search" aria-label={text("Search archived sessions", "搜索已归档会话")} placeholder={text("Search archived sessions", "搜索已归档会话")} disabled={busy} value={query} onChange={(event) => setQuery(event.target.value)} />
      <FieldSelect label={text("Filter by workspace", "按工作区筛选")} value={cwd} disabled={busy}
        onChange={(selectedValue) => setCwd(selectedValue)} options={[
          { value: "", label: text("All workspaces", "所有工作区") },
          ...[...workspaces].map((path) => ({ value: path, label: `${workspaceName(path)} — ${path}` })),
        ]} />
      <Button size="sm" variant="secondary" isDisabled={!connected || busy} onPress={() => { void refresh() }}><RotateCw size={15} /> {text("Refresh", "刷新")}</Button>
    </div>
    {error && <p className="rw-error" role="alert">{error}</p>}
    {report && <p role="status">{report}</p>}
    {!connected && <p className="rw-muted" role="status">{text("The App Server is disconnected.", "应用服务器已断开连接。")}</p>}
    {busy && <p className="rw-muted" role="status">{text("Working…", "处理中…")}</p>}
    {listingValid && visible.length === 0 && <p className="rw-muted" role="status">{archived.length === 0 ? text("No archived sessions", "没有已归档会话") : text("No matching sessions", "没有匹配的会话")}</p>}
    <ul>
      {visible.map((row) => <li key={row.sessionId} className="rw-archive-row">
        <div>
          <strong>{titleFor(row)}</strong>
          {!row.title && !row.preview && <small>{row.sessionId}</small>}
          {row.title && row.preview && row.preview !== row.title && <small>{row.preview}</small>}
          <small>{row.cwd}</small>
          {row.running && <small>{text("Running", "正在运行")}</small>}
          {rowErrors[row.sessionId] && <p className="rw-error" role="alert">{rowErrors[row.sessionId]}</p>}
        </div>
        <div className="rw-setting-options">
          {!row.closed && <Button size="sm" variant="tertiary" aria-label={text(`Restore and open ${titleFor(row)}`, `恢复并打开${titleFor(row)}`)} isDisabled={busy} onPress={() => restore(row.sessionId, true)}>{text("Restore and open", "恢复并打开")}</Button>}
          <Button size="sm" variant="secondary" aria-label={text(`Restore ${titleFor(row)}`, `恢复${titleFor(row)}`)} isDisabled={busy} onPress={() => restore(row.sessionId)}><ArchiveRestore size={15} /> {text("Restore", "恢复")}</Button>
          <Button size="sm" variant="danger-soft" aria-label={text(`Delete ${titleFor(row)}`, `删除${titleFor(row)}`)} isIconOnly isDisabled={busy || !canDelete || row.running}
            onPress={() => { void deleteRows([row]) }}><Trash2 size={15} /></Button>
        </div>
      </li>)}
    </ul>
    {missing.length > 0 && <p>{text(`${missing.length} unavailable archive markers.`, `${missing.length} 个归档标记不可用。`)} <Button size="sm" variant="tertiary" isDisabled={busy}
      onPress={() => { if (!archive.drop(missing)) setError(storageError) }}>{text("Remove markers", "移除不可用标记")}</Button></p>}
    {deletable.length > 1 && <Button size="sm" variant="danger-soft" isDisabled={busy || !canDelete}
      onPress={() => { void deleteRows(deletable) }}>{text(`Delete ${deletable.length} matching archived sessions`, `删除 ${deletable.length} 个匹配的已归档会话`)}</Button>}
  </section>
}
