import { FieldSelect } from "./field-select"
import { Button } from "@heroui/react"
import { ChevronRight, RefreshCw } from "lucide-react"
import { useEffect, useState } from "react"
import { readSkills, type SkillEntry, type SkillsView } from "../rpc/general-settings"
import { readAck } from "../rpc/read"
import { connection, sessions, useStoreValue } from "./stores"

const readPhase = () => connection.phase
const readWorkspaces = () => sessions.workspaces

export function SkillSettings({ text, detail }: {
  text: (en: string, zh: string) => string
  detail?: string
}) {
  const phase = useStoreValue(readPhase)
  const workspaces = useStoreValue(readWorkspaces)
  const [loaded, setLoaded] = useState<{ cwd: string; catalog: SkillsView }>()
  const [search, setSearch] = useState("")
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<{ message: string; path: string; refresh: boolean }>()
  const [status, setStatus] = useState("")
  const cwd = loaded?.cwd ?? ""
  const catalog = loaded?.catalog
  const query = search.trim().toLowerCase()
  const entries = catalog?.entries.filter(({ name, description, filePath }) =>
    `${name}\n${description}\n${filePath}`.toLowerCase().includes(query)) ?? []
  const connected = phase === "connected"
  const disabled = busy || !connected
  const canRefresh = connection.supports("skills.refresh")
  const canToggle = connection.supports("skills.set_enabled")

  async function load(path = cwd, action?: "refresh" | SkillEntry) {
    if (disabled) return
    setBusy(true)
    setError(undefined)
    setStatus("")
    try {
      if (!connection.supports("skills.catalog")) throw new Error(text("Skills management is unavailable on this server.", "当前服务器不支持技能管理。"))
      const scope = path ? { cwd: path } : {}
      if (action === "refresh") await connection.client.call("skills.refresh", scope, readAck)
      else if (action !== undefined) {
        await connection.client.call("skills.set_enabled", { ...scope, id: action.id, enabled: !action.enabled }, readAck)
      }
      const result = await connection.client.call("skills.catalog", scope, readSkills)
      setLoaded({ cwd: path, catalog: result })
      if (action === "refresh") setStatus(text("Skills reloaded.", "技能已重新加载。"))
    } catch (cause) {
      setError({ message: cause instanceof Error ? cause.message : String(cause), path, refresh: action === "refresh" })
    } finally {
      setBusy(false)
    }
  }

  useEffect(() => {
    if (connected) void load()
  }, [connected])

  return <article className="rw-settings">
    <h1>{text("Skills", "技能")}</h1>
    <div className="rw-field-row">
      <div className="rw-select-field">
        {text("Scope", "范围")}
        <FieldSelect label={text("Scope", "范围")} value={cwd} disabled={disabled}
          onChange={(selectedValue) => void load(selectedValue)} options={[
            { value: "", label: text("User skills", "用户技能") },
            ...(cwd && !workspaces.some((workspace) => workspace.cwd === cwd) ? [{ value: cwd, label: cwd }] : []),
            ...workspaces.map(({ name, cwd }) => ({ value: cwd, label: `${name} — ${cwd}` })),
          ]} />
      </div>
      <label className="rw-setting-language">
        {text("Search skills", "搜索技能")}
        <input type="search" value={search} onChange={(event) => setSearch(event.target.value)} />
      </label>
      <Button size="sm" variant="secondary" isDisabled={disabled || !canRefresh} onPress={() => void load(cwd, "refresh")}>
        <RefreshCw size={15} /> {text("Reload skills", "重新加载技能")}
      </Button>
    </div>
    {busy && <p role="status" className="rw-muted">{text("Loading skills…", "正在加载技能…")}</p>}
    {error && <div className="rw-settings-card">
      <p className="rw-error" role="alert">{error.message}</p>
      <Button size="sm" variant="secondary" isDisabled={disabled} onPress={() => void load(error.path, error.refresh ? "refresh" : undefined)}>{text("Retry", "重试")}</Button>
    </div>}
    {status && <p role="status">{status}</p>}
    {catalog?.issues.map(({ path, message }) => <p key={JSON.stringify([path, message])} className="rw-skills-issue">
      <code>{path}</code> · {message}
    </p>)}
    {entries.length > 0 ? <ul className="rw-skills-list">
      {entries.map((skill) => {
        const action = skill.enabled ? text("Disable", "禁用") : text("Enable", "启用")
        const state = skill.shadowedBy ? text("Shadowed", "已被覆盖") : skill.enabled ? text("Enabled", "已启用") : text("Disabled", "已禁用")
        return <li key={skill.filePath}>
          <div className="rw-skill-heading">
            <h2>{skill.name}</h2><small>{state}</small>
            <Button size="sm" variant="secondary" aria-label={`${action} ${skill.name}`}
              isDisabled={disabled || skill.shadowedBy !== undefined || !canToggle} onPress={() => void load(cwd, skill)}>{action}</Button>
          </div>
          <p>{skill.description}</p>
          {skill.shadowedBy && <p>{text("Shadowed by", "被以下技能覆盖")} · <code>{skill.shadowedBy}</code></p>}
          <details open={skill.id === detail ? true : undefined}>
            <summary><ChevronRight size={14} /> {text("Details", "详情")}</summary>
            <code>{skill.filePath}</code>
            {skill.allowedTools.length > 0 && <p>{text("Declared tools", "声明的工具")} · {skill.allowedTools.join(", ")}</p>}
          </details>
        </li>
      })}
    </ul> : catalog && <p role="status" className="rw-muted">{catalog.entries.length > 0
      ? text("No matching skills.", "没有匹配的技能。")
      : text("No installed skills found.", "未发现已安装的技能。")}</p>}
  </article>
}
