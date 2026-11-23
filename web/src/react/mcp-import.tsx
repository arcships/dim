import { FieldSelect } from "./field-select"
import { Button } from "@heroui/react"
import { useRef, useState } from "react"
import { RpcError } from "../rpc/client"
import {
  readMcpImportApplied, readMcpImportPreview, readMcpImportSources,
  type McpImportAction, type McpImportPreview, type McpImportResolution, type McpImportSource,
} from "../rpc/general-settings"
import { connection } from "./stores"

type ImportInput = { document: string } | { from: string }
type ImportSelection = { input: ImportInput; report: McpImportPreview; resolutions: McpImportResolution[] }

export function McpImport({ cwd, text, disabled, permitted, operate, refresh }: {
  cwd: string
  text: (en: string, zh: string) => string
  disabled: boolean
  permitted: (method: string) => boolean
  operate: (operation: () => Promise<void>) => Promise<void>
  refresh: () => Promise<unknown>
}) {
  const fileInput = useRef<HTMLInputElement>(null)
  const [sources, setSources] = useState<McpImportSource[]>()
  const [preview, setPreview] = useState<ImportSelection>()
  const [stale, setStale] = useState(false)
  const [notice, setNotice] = useState("")
  const target = cwd ? { cwd, source: "project" } : { source: "user" }
  const allowed = (method: string) => !disabled && permitted(method)

  async function readPreview(input: ImportInput) {
    const report = await connection.client.call("mcp.import.preview", { ...target, ...input }, readMcpImportPreview)
    const resolutions = report.entries.map(({ id, status }): McpImportResolution =>
      ({ id, action: status === "changed" || status === "identical" ? "import" : "skip" }))
    setPreview({ input, report, resolutions })
    setStale(false)
    setSources(undefined)
  }

  function setResolution(id: string, change: Partial<McpImportResolution>) {
    setPreview((current) => current && {
      ...current,
      resolutions: current.resolutions.map((entry) => entry.id === id ? { ...entry, ...change } : entry),
    })
  }

  const ready = preview !== undefined && preview.resolutions.length > 0
    && preview.resolutions.every(({ action, newId }) => action !== "rename" || !!newId?.trim())

  function apply() {
    if (!preview || !ready || stale || !allowed("mcp.import.apply")) return
    void operate(async () => {
      setNotice("")
      const { input, report } = preview
      const resolutions = preview.resolutions.map(({ id, action, newId }): McpImportResolution => ({
        id, action, ...(action === "rename" ? { newId: newId?.trim() } : {}),
      }))
      try {
        await connection.client.call("mcp.import.apply", {
          ...target, ...input, expectedVersion: report.revision, resolutions,
          ...("from" in input ? { expectedSourceRevision: report.sourceRevision } : {}),
        }, readMcpImportApplied)
      } catch (cause) {
        if (!(cause instanceof RpcError) || cause.appError !== "mcp_settings_conflict") throw cause
        setStale(true)
        await refresh()
        return
      }
      setPreview(undefined)
      const count = resolutions.filter(({ action }) => action !== "skip").length
      setNotice(text(
        `Import applied to ${count} selected entries. New and replaced entries start disabled.`,
        `已应用 ${count} 项导入选择。新增和替换的条目默认禁用。`,
      ))
      await refresh()
    })
  }

  return <>
    <section className="rw-settings-card">
      <h2>{text("Import MCP configuration", "导入 MCP 配置")}</h2>
      <p>{cwd ? text(`Destination · Project ${cwd}`, `导入到项目 · ${cwd}`) : text("Destination · User configuration", "导入到 · 用户配置")}</p>
      <input ref={fileInput} type="file" accept="application/json,.json" hidden onChange={(event) => {
        const file = event.target.files?.[0]
        event.target.value = ""
        if (!file || !allowed("mcp.import.preview")) return
        void operate(async () => {
          setNotice("")
          await readPreview({ document: await file.text() })
        })
      }} />
      <div className="rw-setting-options">
        <Button size="sm" variant="secondary" isDisabled={!allowed("mcp.import.preview")} onPress={() => fileInput.current?.click()}>
          {text("Import from file…", "从文件导入…")}
        </Button>
        <Button size="sm" variant="secondary" isDisabled={!allowed("mcp.import.sources")} onPress={() => void operate(async () => {
          setNotice("")
          setSources(await connection.client.call("mcp.import.sources", cwd ? { cwd } : {}, readMcpImportSources))
        })}>{text("Discover…", "发现…")}</Button>
      </div>
    </section>
    {notice && <p role="status">{notice}</p>}
    {sources && <section className="rw-settings-card">
      <h2>{text("MCP stores of other local clients", "其他本地客户端的 MCP 存储")}</h2>
      {sources.map((store) => <div className="rw-mcp-import-entry" key={store.id}>
        <p><strong>{store.id}</strong> · {store.client} · {store.scope} · {store.state}</p>
        <p><code>{store.path}</code></p>
        {store.error && <p className="rw-error">{store.error}</p>}
        {store.state === "ready" && <>
          <p>{text("Entries", "条目")} · {store.entries}</p>
          <Button size="sm" variant="secondary" isDisabled={!allowed("mcp.import.preview")}
            onPress={() => void operate(() => readPreview({ from: store.id }))}>{text("Preview", "预览")}</Button>
        </>}
      </div>)}
      {sources.length === 0 && <p role="status">{text("No known stores found.", "未找到已知存储。")}</p>}
      <Button size="sm" variant="secondary" isDisabled={disabled} onPress={() => setSources(undefined)}>{text("Close discovery", "关闭发现")}</Button>
    </section>}
    {preview && <section className="rw-settings-card rw-settings-fields">
      <h2>{text("Import preview", "导入预览")}</h2>
      {preview.report.store && <p>{preview.report.store.client} · {preview.report.store.path}</p>}
      {cwd && <p>{text("The project file receives public fields. Arguments, credentials and other withheld values stay in user configuration.",
        "项目文件接收公开字段。参数、凭据和其他隐藏值留在用户配置中。")}</p>}
      <p>{text("Top-level fields omitted", "省略的顶层字段")} · {preview.report.unknownTopLevel.join(", ") || text("None", "无")}</p>
      {stale && <div role="alert" className="rw-mcp-import-entry">
        <p className="rw-error">{text(
          "The source changed. No entry was imported, and a target entry may now be disabled. Preview again before applying.",
          "来源已更改。未导入条目，目标条目可能已禁用。请重新预览后再应用。",
        )}</p>
        <Button size="sm" variant="secondary" isDisabled={!allowed("mcp.import.preview")}
          onPress={() => void operate(() => readPreview(preview.input))}>{text("Preview again", "重新预览")}</Button>
      </div>}
      {preview.report.entries.map((entry, index) => {
        const { action, newId } = preview.resolutions[index]!
        return <div className="rw-mcp-import-entry" key={entry.id}>
          <h3>{entry.id} · {entry.status}{entry.transport && ` · ${entry.transport}`}</h3>
          {entry.changes.length > 0 && <p>{text("Changed fields", "变更字段")} · {entry.changes.map(({ field, kind }) => `${field} (${kind})`).join(", ")}</p>}
          {entry.unsupported.length > 0 && <p>{text("Unsupported fields", "不支持的字段")} · {entry.unsupported.map(({ field, effect }) => `${field} (${effect})`).join(", ")}</p>}
          {entry.omitted > 0 && <p>{text(`${entry.omitted} more field names omitted.`, `另有 ${entry.omitted} 个字段名未列出。`)}</p>}
          {entry.error && <p className="rw-error">{entry.error}</p>}
          {entry.status === "invalid" ? <p>{text("This entry can only be skipped.", "此条目只能跳过。")}</p> : <>
            <div className="rw-select-field">{text("Import action", "导入操作")}
              <FieldSelect<McpImportAction> label={`${text("Import action", "导入操作")} · ${entry.id}`} value={action} disabled={disabled || stale || !allowed("mcp.import.apply")}
                onChange={(action) => setResolution(entry.id, { action, newId: undefined })} options={[
                  { value: "import", label: text("Import", "导入") },
                  { value: "skip", label: text("Skip", "跳过") },
                  { value: "replace", label: text("Replace existing entry", "替换现有条目") },
                  { value: "rename", label: text("Import under a new ID", "以新 ID 导入") },
                ]} />
            </div>
            {action === "rename" && <label>{text("New ID", "新 ID")}
              <input value={newId ?? ""} disabled={!allowed("mcp.import.apply") || stale}
                onChange={(event) => setResolution(entry.id, { newId: event.target.value })} />
            </label>}
          </>}
        </div>
      })}
      {preview.report.entries.length === 0 && <p role="status">{text("No mcpServers entries found.", "未找到 mcpServers 条目。")}</p>}
      <div className="rw-setting-options">
        <Button size="sm" isDisabled={!ready || stale || !allowed("mcp.import.apply")} onPress={apply}>{text("Apply import", "应用导入")}</Button>
        <Button size="sm" variant="secondary" onPress={() => setPreview(undefined)}>{text("Close preview", "关闭预览")}</Button>
      </div>
    </section>}
  </>
}
