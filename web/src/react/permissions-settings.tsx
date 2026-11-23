import { FieldSelect } from "./field-select"
import { Button } from "@heroui/react"
import { Plus, Trash2 } from "lucide-react"
import { useEffect, useLayoutEffect, useState, type FormEvent } from "react"
import { RpcError } from "../rpc/client"
import {
  readExecutionSettings, readRevision, readRules, readSandboxBackend,
  type CommandRule, type ExecutionSettings, type RulesView, type SandboxBackend,
} from "../rpc/general-settings"
import { connection, useStoreValue } from "./stores"

const readPhase = () => connection.phase
type Isolation = Pick<ExecutionSettings, "sandbox" | "network">

function sameRule(a: CommandRule, b: CommandRule) {
  return a.command === b.command && a.match === b.match && a.decision === b.decision
}

export function PermissionsSettings({ text, onDirty }: {
  text: (en: string, zh: string) => string
  onDirty: (dirty: boolean) => void
}) {
  const phase = useStoreValue(readPhase)
  const [rulesView, setRulesView] = useState<RulesView>()
  const [rules, setRules] = useState<CommandRule[]>([])
  const [execution, setExecution] = useState<ExecutionSettings>()
  const [isolation, setIsolation] = useState<Isolation>({ sandbox: false, network: false })
  const [backend, setBackend] = useState<SandboxBackend>()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [isolationError, setIsolationError] = useState("")
  const [rulesConflict, setRulesConflict] = useState(false)
  const [isolationConflict, setIsolationConflict] = useState(false)
  const [status, setStatus] = useState("")
  const connected = phase === "connected"
  const disabled = busy || !connected
  const canSetRules = connection.supports("permission.set_rules")
  const canSetIsolation = connection.supports("settings.update")
  const rulesDirty = rulesView !== undefined && (rules.length !== rulesView.commandRules.length
    || rules.some((rule, index) => !sameRule(rule, rulesView.commandRules[index]!)))
  const isolationDirty = execution !== undefined && (isolation.sandbox !== execution.sandbox || isolation.network !== execution.network)
  const dirty = rulesDirty || isolationDirty

  useLayoutEffect(() => {
    onDirty(dirty)
    return () => onDirty(false)
  }, [dirty, onDirty])

  async function load() {
    if (disabled) return
    if (dirty && !window.confirm(text("Discard unsaved permission settings?", "放弃未保存的权限设置？"))) return
    setBusy(true)
    setError("")
    setIsolationError("")
    setStatus("")
    const [rulesResult, executionResult, backendResult] = await Promise.allSettled([
      connection.supports("permission.rules") ? connection.client.call("permission.rules", {}, readRules)
        : Promise.reject(new Error(text("Command rules are unavailable on this server.", "当前服务器不支持命令规则。"))),
      connection.supports("settings.read") ? connection.client.call("settings.read", { source: "execution", scope: "user" }, readExecutionSettings)
        : Promise.reject(new Error(text("Process isolation settings are unavailable on this server.", "当前服务器不支持进程隔离设置。"))),
      connection.supports("host.execution.backend") ? connection.client.call("host.execution.backend", {}, readSandboxBackend)
        : Promise.reject(new Error(text("Sandbox status is unavailable on this server.", "当前服务器不支持查询沙盒状态。"))),
    ])
    if (rulesResult.status === "fulfilled") {
      setRulesView(rulesResult.value)
      setRules(rulesResult.value.commandRules)
      setRulesConflict(false)
    } else {
      setError(rulesResult.reason instanceof Error ? rulesResult.reason.message : String(rulesResult.reason))
    }
    if (executionResult.status === "fulfilled") {
      setExecution(executionResult.value)
      setIsolation(executionResult.value)
      setIsolationConflict(false)
    }
    if (backendResult.status === "fulfilled") setBackend(backendResult.value)
    else setBackend(undefined)
    const isolationFailure = executionResult.status === "rejected" ? executionResult : backendResult
    if (isolationFailure.status === "rejected") {
      setIsolationError(isolationFailure.reason instanceof Error ? isolationFailure.reason.message : String(isolationFailure.reason))
    }
    setBusy(false)
  }

  useEffect(() => {
    if (connected && !dirty) void load()
  }, [connected])

  async function saveRules(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!rulesView || !rulesDirty || disabled || rulesConflict || !canSetRules) return
    setError("")
    setStatus("")
    if (rules.some(({ command }) => !command.trim() || /[;&|`()<>$\n\r]/.test(command))) {
      setError(text(
        "Enter a simple command without shell separators, substitutions or redirection.",
        "请输入不含 Shell 分隔符、替换或重定向的简单命令。",
      ))
      return
    }
    const removesDeny = rulesView.commandRules.some((previous) => previous.decision === "deny" && !rules.some((rule) => sameRule(previous, rule)))
    if ((removesDeny || rules.some(({ decision }) => decision === "allow")) && !window.confirm(text(
      "Save command rules that may allow additional commands? Denied permission presets still take precedence.",
      "保存可能放行更多命令的规则？权限预设中的拒绝仍然优先。",
    ))) return
    setBusy(true)
    try {
      const revision = await connection.client.call("permission.set_rules", { expectedRevision: rulesView.revision, commandRules: rules }, readRevision)
      setRulesView({ revision, commandRules: rules })
      setStatus(text("Command rules saved.", "命令规则已保存。"))
    } catch (cause) {
      const conflict = cause instanceof RpcError && cause.appError === "permission_rules_conflict"
      setRulesConflict(conflict)
      setError(conflict ? text("Command rules changed. Reload before saving again. Your draft is still here.", "命令规则已变更。请重新加载后再保存，当前草稿已保留。")
        : cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  async function saveIsolation(reset = false) {
    if (!execution || disabled || isolationConflict || !canSetIsolation || (!reset && !isolationDirty)) return
    const document = reset ? { sandbox: false, network: false } : { sandbox: isolation.sandbox, network: isolation.network }
    const widens = execution.sandbox && (!document.sandbox || (!execution.network && document.network))
    if ((reset || widens) && !window.confirm(reset
      ? text("Reset process isolation to Off? New commands will run without sandbox isolation.", "将进程隔离恢复为关闭？新命令将不使用沙盒隔离。")
      : text("Run new commands with less isolation? Running processes keep their current boundary.", "降低新命令的隔离级别？正在运行的进程保留当前边界。"))) return
    if (reset) setIsolation(document)
    setBusy(true)
    setIsolationError("")
    setStatus("")
    try {
      const next = await connection.client.call("settings.update", {
        source: "execution", scope: "user", expectedRevision: execution.revision, document,
      }, readExecutionSettings)
      setExecution(next)
      setIsolation(next)
      setStatus(text("Isolation settings saved.", "隔离设置已保存。"))
    } catch (cause) {
      const conflict = cause instanceof RpcError && cause.appError === "settings_conflict"
      setIsolationConflict(conflict)
      setIsolationError(conflict ? text("Process isolation settings changed. Reload before saving again. Your draft is still here.", "进程隔离设置已变更。请重新加载后再保存，当前草稿已保留。")
        : cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  function editRule(index: number, change: Partial<CommandRule>) {
    setRules((current) => current.map((rule, position) => position === index ? { ...rule, ...change } : rule))
    setStatus("")
  }

  const mode = !isolation.sandbox ? "off" : isolation.network ? "network" : "isolated"
  return <article className="rw-settings">
    <h1>{text("Permissions", "权限")}</h1>
    <Button className="rw-settings-reload" size="sm" variant="secondary" isPending={busy} isDisabled={!connected}
      onPress={() => void load()}>{text("Reload permissions", "重新加载权限")}</Button>
    {status && <p role="status">{status}</p>}
    {isolationError && <p className="rw-error" role="alert">{isolationError}</p>}
    {execution && <form className="rw-settings-card" onSubmit={(event) => { event.preventDefault(); void saveIsolation() }}>
      <h2>{text("Process isolation", "进程隔离")}</h2>
      <p>{text(
        "Applies to new commands on this host. Running processes and persistent shells keep their isolation.",
        "适用于此主机上的新命令。正在运行的进程和持久 Shell 保持原有隔离。",
      )}</p>
      {backend?.available === false && <p className="rw-error">
        {text("Sandbox unavailable on this host", "此主机上沙盒不可用")} · {backend.reason}
      </p>}
      <div className="rw-select-field">
        {text("Isolation mode", "隔离模式")}
        <FieldSelect label={text("Isolation mode", "隔离模式")} value={mode} disabled={disabled || !canSetIsolation}
          onChange={(selectedValue) => {
            const value = selectedValue
            setIsolation((current) => ({ sandbox: value !== "off", network: value === "off" ? current.network : value === "network" }))
            setStatus("")
          }} options={[
            { value: "off", label: text("Off · Host files and network", "关闭 · 使用主机文件和网络") },
            { value: "isolated", label: text("Sandboxed · No network", "沙盒隔离 · 禁止网络") },
            { value: "network", label: text("Sandboxed · Network allowed", "沙盒隔离 · 允许网络") },
          ]} />
      </div>
      <div className="rw-setting-options">
        <Button type="button" size="sm" variant="secondary" isDisabled={disabled || isolationConflict || !canSetIsolation}
          onPress={() => void saveIsolation(true)}>{text("Reset to default", "恢复默认")}</Button>
        <Button type="submit" size="sm" isDisabled={disabled || isolationConflict || !isolationDirty || !canSetIsolation}>
          {text("Save isolation settings", "保存隔离设置")}
        </Button>
      </div>
    </form>}
    {error && <p className="rw-error" role="alert">{error}</p>}
    {rulesView && <form className="rw-settings-card" onSubmit={saveRules}>
      <h2>{text("Command rules", "命令规则")}</h2>
      <p>{text(
        "These user rules apply to process-starting tools in every session. Deny takes precedence over Allow. Rules match command words.",
        "这些用户规则适用于所有会话中启动进程的工具。拒绝优先于允许。规则按命令词匹配。",
      )}</p>
      <div className="rw-permission-rules">
        {rules.map((rule, index) => <div className="rw-field-row rw-permission-rule" key={index}>
          <label className="rw-setting-language">
            {text("Command", "命令")}
            <input required value={rule.command} disabled={disabled || !canSetRules} onChange={(event) => editRule(index, { command: event.target.value })} />
          </label>
          <div className="rw-select-field">
            {text("Match", "匹配方式")}
            <FieldSelect label={text("Match", "匹配方式")} value={rule.match} disabled={disabled || !canSetRules}
              onChange={(selectedValue) => {
                const match = selectedValue
                if (match === "exact" || match === "prefix") editRule(index, { match })
              }} options={[
                { value: "exact", label: text("Exact", "精确") },
                { value: "prefix", label: text("Prefix", "前缀") },
              ]} />
          </div>
          <div className="rw-select-field">
            {text("Decision", "决策")}
            <FieldSelect label={text("Decision", "决策")} value={rule.decision} disabled={disabled || !canSetRules}
              onChange={(selectedValue) => {
                const decision = selectedValue
                if (decision === "allow" || decision === "deny") editRule(index, { decision })
              }} options={[
                { value: "deny", label: text("Deny", "拒绝") },
                { value: "allow", label: text("Allow", "允许") },
              ]} />
          </div>
          <Button type="button" size="sm" variant="tertiary" isIconOnly aria-label={`${text("Remove rule", "移除规则")} ${index + 1}`}
            isDisabled={disabled || !canSetRules} onPress={() => {
              setRules((current) => current.filter((_, position) => position !== index))
              setStatus("")
            }}><Trash2 size={15} /></Button>
        </div>)}
      </div>
      {rules.length === 0 && <p>{text("No command rules.", "尚无命令规则。")}</p>}
      <div className="rw-setting-options">
        <Button type="button" size="sm" variant="secondary" isDisabled={disabled || !canSetRules} onPress={() => {
          setRules((current) => [...current, { command: "", match: "prefix", decision: "deny" }])
          setStatus("")
        }}><Plus size={15} /> {text("Add rule", "添加规则")}</Button>
        <Button type="submit" size="sm" isDisabled={disabled || rulesConflict || !rulesDirty || !canSetRules}>{text("Save rules", "保存规则")}</Button>
      </div>
    </form>}
  </article>
}
