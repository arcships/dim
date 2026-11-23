import { FieldSelect } from "./field-select"
import { Button } from "@heroui/react"
import { useEffect, useLayoutEffect, useState, type FormEvent } from "react"
import { readPlugins, type PluginEntry } from "../rpc/general-settings"
import { readAck } from "../rpc/read"
import { connection, useStoreValue } from "./stores"

const readOnline = () => connection.phase === "connected"
const installTimeout = 180_000
const optionalInstallTimeout = 15 * 60_000

type PluginAction =
  | { method: "plugin.install"; params: { source: { path: string } | { git: string; ref?: string } | { optional: string }; id?: string; enable?: boolean } }
  | { method: "plugin.enable"; params: { name: string; use: string } }
  | { method: "plugin.disable" | "plugin.reload"; params: { name: string } }
  | { method: "plugin.update" | "plugin.remove"; params: { id: string } }

export function PluginSettings({ text, detail, onDirty }: {
  text: (en: string, zh: string) => string
  detail?: string
  onDirty: (dirty: boolean) => void
}) {
  const online = useStoreValue(readOnline)
  const [plugins, setPlugins] = useState<PluginEntry[]>()
  const [choices, setChoices] = useState<Record<string, string>>({})
  const [sourceType, setSourceType] = useState<"path" | "git">("path")
  const [source, setSource] = useState("")
  const [gitRef, setGitRef] = useState("")
  const [installId, setInstallId] = useState("")
  const [enable, setEnable] = useState(false)
  const [query, setQuery] = useState(detail ?? "")
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [status, setStatus] = useState("")
  const dirty = Boolean(source || gitRef || installId || enable)
  const entries = plugins?.filter(({ name }) => name.toLowerCase().includes(query.trim().toLowerCase()))
  const permitted = (method: string) => online && !busy && connection.supports(method)

  useLayoutEffect(() => {
    onDirty(dirty)
    return () => onDirty(false)
  }, [dirty, onDirty])

  useEffect(() => setQuery(detail ?? ""), [detail])

  async function read() {
    if (!connection.supports("plugin.list")) throw new Error(text("Plugin management is unavailable on this server.", "当前服务器不支持插件管理。"))
    const next = await connection.client.call("plugin.list", {}, readPlugins)
    setPlugins(next)
    setChoices(Object.fromEntries(next.map(({ name, use, candidates }) => [name, use ?? candidates[0]?.id ?? ""])))
  }

  async function refresh() {
    if (!online || busy) return
    setBusy(true)
    setError("")
    setStatus("")
    try {
      await read()
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  useEffect(() => {
    if (online) void refresh()
  }, [online])

  async function act(action: PluginAction) {
    const { method, params } = action
    if (!permitted(method)) return
    if (method === "plugin.remove") {
      if (!window.confirm(text("Remove this installed plugin? Its registered content will be withdrawn.", "移除此已安装插件？它注册的内容将被撤回。"))) return
    } else if (method !== "plugin.disable" && !window.confirm(text(
      "Plugins can execute code and configured commands with your user privileges. Continue?",
      "插件可以使用您的用户权限执行代码和配置命令。是否继续？",
    ))) return

    setBusy(true)
    setError("")
    setStatus("")
    const optionalInstall = action.method === "plugin.install" && "optional" in action.params.source
    try {
      await connection.client.call(method, params, readAck, optionalInstall ? optionalInstallTimeout : installTimeout)
      if (action.method === "plugin.install" && !optionalInstall) {
        setSource("")
        setGitRef("")
        setInstallId("")
        setEnable(false)
      }
      setStatus(text("Plugin operation accepted.", "插件操作已接受。"))
      await read()
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  function install(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const location = source.trim()
    if (!location) return
    const ref = gitRef.trim()
    const id = installId.trim()
    void act({
      method: "plugin.install",
      params: {
        source: sourceType === "path" ? { path: location } : { git: location, ...(ref ? { ref } : {}) },
        ...(id ? { id } : {}),
        enable,
      },
    })
  }

  return <article className="rw-settings">
    <h1>{text("Plugins", "插件")}</h1>
    <div className="rw-field-row">
      <label className="rw-setting-language">
        {text("Search plugins", "搜索插件")}
        <input type="search" value={query} onChange={(event) => setQuery(event.target.value)} />
      </label>
      <Button size="sm" variant="secondary" isPending={busy} isDisabled={!online} onPress={() => void refresh()}>{text("Refresh plugins", "刷新插件")}</Button>
    </div>
    {error && <p className="rw-error" role="alert">{error}</p>}
    {status && <p role="status">{status}</p>}
    {entries?.map((plugin) => {
      const choice = choices[plugin.name] ?? ""
      const optional = plugin.optional === true && plugin.candidates.length === 0
      return <section className="rw-settings-card" key={plugin.name} aria-label={plugin.name}>
        <h2>{plugin.name} {plugin.locked && <small className="rw-muted">{text("Locked", "已锁定")}</small>}</h2>
        <p>{optional ? text("Available to install", "可安装") : plugin.status ?? (plugin.off ? text("Disabled", "已禁用") : text("Content", "内容"))}
          {plugin.use && <> · {plugin.use}</>}</p>
        {plugin.error && <p className="rw-error" role="alert">{plugin.error}</p>}
        {plugin.refused.map(({ kind, error }) => <p className="rw-error" key={`${kind}/${error}`}>{kind} · {error}</p>)}
        {plugin.candidates.length > 0 && <div className="rw-select-field">
          {text("Implementation", "实现")}
          <FieldSelect label={text("Implementation", "实现")} value={choice} disabled={!online || busy || plugin.locked}
            onChange={(selectedValue) => setChoices({ ...choices, [plugin.name]: selectedValue })} options={[
              ...(choice && !plugin.candidates.some(({ id }) => id === choice) ? [{ value: choice, label: choice, disabled: true }] : []),
              ...plugin.candidates.map(({ id, kinds }) => ({ value: id, label: `${id}${kinds.length > 0 ? ` (${kinds.join(", ")})` : ""}` })),
            ]} />
        </div>}
        <div className="rw-setting-options">
          {optional ? <Button size="sm" isDisabled={!permitted("plugin.install")} onPress={() => void act({
            method: "plugin.install", params: { source: { optional: plugin.name } },
          })}>{text("Install", "安装")}</Button> : <>
            <Button size="sm" variant="secondary" isDisabled={plugin.locked || !choice || !plugin.candidates.some(({ id }) => id === choice) || !permitted("plugin.enable")}
              onPress={() => void act({ method: "plugin.enable", params: { name: plugin.name, use: choice } })}>{text("Enable / use", "启用 / 使用")}</Button>
            <Button size="sm" variant="secondary" isDisabled={plugin.locked || plugin.off || !permitted("plugin.disable")}
              onPress={() => void act({ method: "plugin.disable", params: { name: plugin.name } })}>{text("Disable", "禁用")}</Button>
            <Button size="sm" variant="secondary" isDisabled={plugin.off || !permitted("plugin.reload")}
              onPress={() => void act({ method: "plugin.reload", params: { name: plugin.name } })}>{text("Reload", "重新加载")}</Button>
          </>}
        </div>
        {plugin.candidates.filter(({ id }) => id !== "builtin" && id !== "default").map(({ id, git, optionalSource }) => <div className="rw-plugin-candidate" key={id}>
          <code>{id}</code>
          {(git || optionalSource) && <Button size="sm" variant="secondary" isDisabled={!permitted("plugin.update")}
            onPress={() => void act({ method: "plugin.update", params: { id } })}>{text("Update", "更新")}</Button>}
          <Button size="sm" variant="danger-soft" isDisabled={plugin.locked || !permitted("plugin.remove")}
            onPress={() => void act({ method: "plugin.remove", params: { id } })}>{text("Remove", "移除")}</Button>
        </div>)}
      </section>
    })}
    {entries?.length === 0 && <p role="status">{text("No matching plugins.", "没有匹配的插件。")}</p>}
    <form className="rw-settings-card rw-settings-fields" onSubmit={install}>
      <h2>{text("Install plugin", "安装插件")}</h2>
      <fieldset className="rw-plugin-source" disabled={!permitted("plugin.install")}>
        <div className="rw-select-field">{text("Source type", "来源类型")}
          <FieldSelect label={text("Source type", "来源类型")} value={sourceType}
            onChange={(selectedValue) => {
              setSourceType(selectedValue === "git" ? "git" : "path")
              setGitRef("")
            }} options={[
              { value: "path", label: text("Local directory", "本地目录") },
              { value: "git", label: "Git" },
            ]} />
        </div>
        <label>{text("Source", "来源")}
          <input value={source} onChange={(event) => setSource(event.target.value)} required />
        </label>
        {sourceType === "git" && <label>{text("Git ref (optional)", "Git 引用（可选）")}
          <input value={gitRef} onChange={(event) => setGitRef(event.target.value)} />
        </label>}
        <label>{text("Installation ID (optional)", "安装 ID（可选）")}
          <input value={installId} onChange={(event) => setInstallId(event.target.value)} />
        </label>
        <label className="rw-recording-toggle"><input type="checkbox" checked={enable} onChange={(event) => setEnable(event.target.checked)} />
          {text("Enable after installation", "安装后启用")}
        </label>
        <Button size="sm" type="submit" isDisabled={!source.trim()}>{text("Install", "安装")}</Button>
      </fieldset>
    </form>
  </article>
}
