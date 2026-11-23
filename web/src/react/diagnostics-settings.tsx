import { Button } from "@heroui/react"
import { RefreshCw } from "lucide-react"
import { useEffect, useLayoutEffect, useState, type FormEvent } from "react"
import { readModelRecording, readPing, type ModelRecording } from "../rpc/general-settings"
import { connection, useStoreValue } from "./stores"

const readPhase = () => connection.phase
const readHello = () => connection.hello

export function DiagnosticsSettings({ text, onDirty }: {
  text: (en: string, zh: string) => string
  onDirty: (dirty: boolean) => void
}) {
  const phase = useStoreValue(readPhase)
  const hello = useStoreValue(readHello)
  const [ping, setPing] = useState<ReturnType<typeof readPing>>()
  const [latency, setLatency] = useState<number>()
  const [recording, setRecording] = useState<ModelRecording>()
  const [enabled, setEnabled] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [saved, setSaved] = useState(false)
  const [search, setSearch] = useState("")
  const dirty = recording !== undefined && enabled !== recording.enabled
  const canSave = phase === "connected" && connection.supports("settings.update")
  const features = hello?.features.filter((method) => method.toLowerCase().includes(search.trim().toLowerCase())).sort() ?? []

  useLayoutEffect(() => {
    onDirty(dirty)
    return () => onDirty(false)
  }, [dirty, onDirty])

  async function refresh() {
    if (busy || phase !== "connected") return
    if (dirty && !window.confirm(text("Discard unsaved recording settings?", "放弃未保存的录制设置？"))) return
    setBusy(true)
    setError("")
    setSaved(false)
    try {
      const start = performance.now()
      setPing(await connection.client.call("daemon.ping", {}, readPing))
      setLatency(Math.round(performance.now() - start))
      if (connection.supports("settings.read")) {
        const next = await connection.client.call("settings.read", { source: "model-log", scope: "user" }, readModelRecording)
        setRecording(next)
        setEnabled(next.enabled)
      } else {
        setRecording(undefined)
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  useEffect(() => {
    if (phase === "connected" && !dirty) void refresh()
  }, [phase])

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!recording || !dirty || busy || !canSave) return
    if (enabled && !window.confirm(text(
      "Record model requests and responses? Recordings include prompts, source code and tool output. Custom headers may contain secrets. The file has no automatic rotation.",
      "录制模型请求和响应？录制包含提示词、源代码和工具输出。自定义请求头可能包含机密。文件不会自动轮换。",
    ))) return
    setBusy(true)
    setError("")
    setSaved(false)
    try {
      const next = await connection.client.call("settings.update", {
        source: "model-log",
        scope: "user",
        expectedRevision: recording.revision,
        document: { enabled },
      }, readModelRecording)
      setRecording(next)
      setEnabled(next.enabled)
      setSaved(true)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  return <article className="rw-settings">
    <h1>{text("Diagnostics", "诊断")}</h1>
    {error && <p className="rw-error" role="alert">{error}</p>}
    {saved && !dirty && <p role="status">{text("Recording settings saved.", "录制设置已保存。")}</p>}
    <section className="rw-settings-card">
      <dl className="rw-diagnostics-facts">
        <dt>{text("Server version", "服务器版本")}</dt><dd>{hello?.serverVersion ?? "—"}</dd>
        <dt>{text("Protocol", "协议")}</dt><dd>{hello?.protocolVersion ?? "—"}</dd>
        <dt>{text("Instance", "实例")}</dt><dd><code>{hello?.instanceId ?? "—"}</code></dd>
        <dt>{text("Connection", "连接")}</dt><dd>{phase}</dd>
        {ping && <>
          <dt>{text("Sessions", "会话")}</dt><dd>{ping.sessions}</dd>
          <dt>{text("Connections", "连接数")}</dt><dd>{ping.connections}</dd>
          <dt>{text("Round trip", "往返延迟")}</dt><dd>{latency} ms</dd>
        </>}
      </dl>
      <Button size="sm" variant="secondary" isPending={busy} isDisabled={phase !== "connected"} onPress={() => void refresh()}>
        <RefreshCw size={15} /> {text("Refresh diagnostics", "刷新诊断")}
      </Button>
    </section>
    {recording && <form className="rw-settings-card" onSubmit={save}>
      <h2>{text("Model request recording", "模型请求录制")}</h2>
      <p>{text(
        "Changes apply to new requests. The daemon writes prompts, source code and tool output to its private model-log/recordings.jsonl file. Custom headers may contain secrets. The file has no automatic size limit or rotation.",
        "更改适用于新请求。守护进程将提示词、源代码和工具输出写入私有的 model-log/recordings.jsonl 文件。自定义请求头可能包含机密。该文件没有自动大小限制或轮换。",
      )}</p>
      <label className="rw-recording-toggle">
        <input type="checkbox" checked={enabled} disabled={busy || !canSave} onChange={(event) => setEnabled(event.target.checked)} />
        {text("Record model requests and responses", "录制模型请求和响应")}
      </label>
      <Button type="submit" variant="primary" isDisabled={busy || !dirty || !canSave}>{text("Save recording settings", "保存录制设置")}</Button>
    </form>}
    <section className="rw-settings-card">
      <h2>{text("Process logs", "进程日志")}</h2>
      <p>{text(
        "The CLI starts the daemon with a private, append-only daemon.log. A daemon started directly writes to stderr. RUST_LOG sets the filter at startup and defaults to errors.",
        "通过 CLI 启动的守护进程会追加写入私有的 daemon.log。直接启动时写入 stderr。RUST_LOG 在启动时设置过滤条件，默认为仅记录错误。",
      )}</p>
      <p>{text("To change the filter, run dimcode daemon stop, then launch with a filter such as RUST_LOG=debug dimcode.", "如需更改过滤条件，请运行 dimcode daemon stop，再带过滤条件启动，例如 RUST_LOG=debug dimcode。")}</p>
    </section>
    <section className="rw-settings-card">
      <h2>{text("Advertised methods", "已声明的方法")} ({hello?.features.length ?? 0})</h2>
      <label className="rw-setting-language">
        {text("Filter methods", "筛选方法")}
        <input type="search" value={search} onChange={(event) => setSearch(event.target.value)} />
      </label>
      {features.length > 0
        ? <ul className="rw-diagnostics-methods">{features.map((method) => <li key={method}><code>{method}</code></li>)}</ul>
        : <p>{text("No matching methods.", "没有匹配的方法。")}</p>}
    </section>
  </article>
}
