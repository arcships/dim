import { FieldSelect } from "./field-select"
import { Button } from "@heroui/react"
import { useEffect, useLayoutEffect, useState, type FormEvent } from "react"
import {
  readDefaultSelection, readExecShell, readModelPolicy, readRevision, readSelectedModel,
  type DefaultSelection, type ExecShellView, type ModelPolicy, type ModelPolicyView,
} from "../rpc/general-settings"
import type { ModelId } from "../rpc/protocol"
import { ModelPicker } from "./composer-controls"
import { connection, sessions, useStoreValue } from "./stores"

const readPhase = () => connection.phase
const readWorkspaces = () => sessions.workspaces

type Defaults = { cwd: string; selection?: DefaultSelection; policy?: ModelPolicyView; shell?: ExecShellView }

function sameModel(a: ModelId | undefined, b: ModelId | undefined) {
  return a?.providerId === b?.providerId && a?.modelId === b?.modelId
}

export function AgentSettings({ text, onDirty }: {
  text: (en: string, zh: string) => string
  onDirty: (dirty: boolean) => void
}) {
  const phase = useStoreValue(readPhase)
  const workspaces = useStoreValue(readWorkspaces)
  const [defaults, setDefaults] = useState<Defaults>()
  const [model, setModel] = useState<ModelId>()
  const [policy, setPolicy] = useState<ModelPolicy>({ mode: "agent" })
  const [shellPath, setShellPath] = useState("")
  const [busy, setBusy] = useState(false)
  const [saved, setSaved] = useState(false)
  const [error, setError] = useState("")
  const connected = phase === "connected"
  const cwd = defaults?.cwd ?? ""
  const configuredPolicy = defaults?.policy?.policy
  const modelChanged = !sameModel(model, defaults?.selection)
  const policyChanged = configuredPolicy !== undefined && (
    configuredPolicy.mode !== policy.mode
    || (configuredPolicy.mode === "fixed" && policy.mode === "fixed" && !sameModel(configuredPolicy.model, policy.model))
  )
  const shellChanged = defaults?.shell !== undefined && shellPath !== (defaults.shell.executablePath ?? "")
  const dirty = defaults !== undefined && (modelChanged || policyChanged || shellChanged)

  useLayoutEffect(() => {
    onDirty(dirty)
    return () => onDirty(false)
  }, [dirty, onDirty])

  async function load(path = cwd) {
    if (busy || !connected) return
    if (dirty && !window.confirm(text("Discard unsaved defaults?", "放弃未保存的默认设置？"))) return
    setBusy(true)
    setError("")
    setSaved(false)
    try {
      if (!connection.supports("provider.selection")) throw new Error(text("Default model settings are unavailable on this server.", "当前服务器不支持默认模型设置。"))
      const [selection, modelPolicy, shell] = await Promise.all([
        connection.client.call("provider.selection", path ? { cwd: path } : {}, readDefaultSelection),
        connection.supports("collaboration.model_policy") ? connection.client.call("collaboration.model_policy", {}, readModelPolicy) : undefined,
        connection.supports("host.exec_shell.get") ? connection.client.call("host.exec_shell.get", {}, readExecShell) : undefined,
      ])
      setDefaults({ cwd: path, selection, policy: modelPolicy, shell })
      setModel(selection)
      setPolicy(modelPolicy?.policy ?? { mode: "agent" })
      setShellPath(shell?.executablePath ?? "")
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  useEffect(() => {
    if (connected && !dirty) void load()
  }, [connected])

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!defaults || !dirty || busy || !connected) return
    setBusy(true)
    setError("")
    setSaved(false)
    try {
      if (model && modelChanged) {
        const selection = await connection.client.call("provider.select", {
          ...(cwd ? { cwd } : {}),
          providerId: model.providerId,
          modelId: model.modelId,
          ...(defaults.selection ? { expectedVersion: defaults.selection.version } : {}),
        }, readSelectedModel)
        setDefaults((previous) => previous && { ...previous, selection })
        setModel(selection)
      }
      if (defaults.policy && policyChanged) {
        const revision = await connection.client.call("collaboration.set_model_policy", {
          expectedRevision: defaults.policy.revision,
          policy,
        }, readRevision)
        setDefaults((previous) => previous && { ...previous, policy: { revision, policy } })
      }
      if (defaults.shell && shellChanged) {
        const shell = await connection.client.call("host.exec_shell.update", {
          expectedRevision: defaults.shell.revision,
          ...(shellPath ? { executablePath: shellPath } : {}),
        }, readExecShell)
        setDefaults((previous) => previous && { ...previous, shell })
        setShellPath(shell.executablePath ?? "")
      }
      setSaved(true)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  const disabled = busy || !connected
  const canChooseModel = connection.supports("provider.models.query")
  const canSetPolicy = connection.supports("collaboration.set_model_policy")
  const fixedModel = policy.mode === "fixed" ? policy.model : undefined
  const shell = defaults?.shell
  return <article className="rw-settings">
    <h1>{text("Agent", "代理")}</h1>
    <section className="rw-settings-card">
      <div className="rw-select-field">
        {text("Default model scope", "默认模型范围")}
        <FieldSelect label={text("Default model scope", "默认模型范围")} value={cwd} disabled={disabled || !defaults}
          onChange={(selectedValue) => void load(selectedValue)} options={[
            { value: "", label: text("User default · all workspaces", "用户默认 · 所有工作区") },
            ...(cwd && !workspaces.some((workspace) => workspace.cwd === cwd) ? [{ value: cwd, label: cwd }] : []),
            ...workspaces.map((workspace) => ({ value: workspace.cwd, label: `${workspace.name} — ${workspace.cwd}` })),
          ]} />
      </div>
      <Button size="sm" variant="secondary" isPending={busy} isDisabled={!connected} onPress={() => void load()}>
        {text("Reload defaults", "重新加载默认设置")}
      </Button>
    </section>
    {error && <p className="rw-error" role="alert">{error}</p>}
    {saved && !dirty && <p role="status">{text("Defaults saved.", "默认设置已保存。")}</p>}
    {defaults && <form className="rw-settings-form" onSubmit={save}>
      <section className="rw-settings-card">
        <h2>{text("Default model", "默认模型")}</h2>
        <p>{text(
          "A workspace default overrides the user default for new sessions in that workspace. Existing sessions keep their model.",
          "工作区默认值覆盖该工作区新会话的用户默认值。现有会话保留其模型。",
        )}</p>
        <ModelPicker model={model} disabled={disabled || !canChooseModel || !connection.supports("provider.select")}
          choose={setModel} label={text("Choose default model", "选择默认模型")} />
      </section>
      {defaults.policy && <section className="rw-settings-card">
        <h2>{text("Subagent models", "子代理模型")}</h2>
        <p>{text("Applies to new native Collaboration members in all workspaces.", "适用于所有工作区中新建的原生协作成员。")}</p>
        <div className="rw-select-field">
          {text("Model policy", "模型策略")}
          <FieldSelect label={text("Model policy", "模型策略")} value={policy.mode} disabled={disabled || !canSetPolicy}
            onChange={(selectedValue) => {
              const mode = selectedValue
              if (mode === "agent" || mode === "inherit") setPolicy({ mode })
              else if (configuredPolicy?.mode === "fixed") setPolicy(configuredPolicy)
            }} options={[
              { value: "agent", label: text("Agent decides", "由代理决定") },
              { value: "inherit", label: text("Inherit requesting branch model", "继承请求分支的模型") },
              ...((fixedModel || configuredPolicy?.mode === "fixed") ? [{ value: "fixed", label: text("Fixed model", "固定模型") }] : []),
            ]} />
        </div>
        <ModelPicker model={fixedModel} disabled={disabled || !canChooseModel || !canSetPolicy}
          choose={(value) => setPolicy({ mode: "fixed", model: value })}
          label={text("Choose fixed subagent model", "选择固定子代理模型")}
          placeholder={text("Choose fixed model", "选择固定模型")} />
      </section>}
      {shell && <section className="rw-settings-card">
        <h2>{text("Command shell", "命令 Shell")}</h2>
        <p>{text("Applies to new commands in all workspaces. Running processes keep their shell.", "适用于所有工作区中的新命令。正在运行的进程保留其 Shell。")}</p>
        <div className="rw-select-field">
          {text("Executable", "可执行程序")}
          <FieldSelect label={text("Executable", "可执行程序")} value={shellPath} disabled={disabled || !connection.supports("host.exec_shell.update")}
            onChange={(selectedValue) => setShellPath(selectedValue)} options={[
              { value: "", label: text("Automatic detection", "自动检测") },
              ...(shellPath && !shell.shells.some((option) => option.executablePath === shellPath) ? [{ value: shellPath, label: `${shellPath} (${text("unavailable", "不可用")})`, disabled: true }] : []),
              ...shell.shells.map(({ executablePath, name }) => ({ value: executablePath, label: `${name} — ${executablePath}` })),
            ]} />
        </div>
        {shell.configError && <div className="rw-error" role="alert">{shell.configError}</div>}
      </section>}
      <Button type="submit" isDisabled={disabled || !dirty}>{text("Save defaults", "保存默认设置")}</Button>
    </form>}
  </article>
}
