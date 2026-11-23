import { FieldSelect } from "./field-select"
import { Button } from "@heroui/react"
import { useEffect, useLayoutEffect, useRef, useState, type FormEvent } from "react"
import { RpcError } from "../rpc/client"
import { readManagement, readModelWrite, type ManagementModel, type ManagementProvider, type ModelEntry } from "../rpc/management"
import { connection } from "./stores"

type Text = (en: string, zh: string) => string
type Selection = { provider: ManagementProvider; next: string | null }
type Draft = {
  id: string; name: string; context: string; output: string; tools: boolean; vision: boolean; reasoning: boolean
  efforts: string; effort: string; tier: string; endpoint: string; alias: string
}
type Editor = { original?: ManagementModel; initial: Draft; draft: Draft; version: number }
type Change = { action: "set"; value: string } | { action: "clear" }
type ModelEdits = {
  displayName?: string; contextWindow?: number; maxOutputTokens?: number; toolCalling?: boolean; vision?: boolean
  reasoning?: { supported: boolean; effortOptions: string[] }; endpoint?: Change; alias?: Change
}
const field = (value: string): Change => value ? { action: "set", value } : { action: "clear" }
const effortsFor = (value: string) => value.split(",").map((part) => part.trim()).filter(Boolean)
const endpoints = ["openai", "openai-response", "openai-response-compact", "anthropic", "gemini"]

function modelDraft(entry: ManagementModel | undefined, provider: ManagementProvider): Draft {
  const model = entry?.model
  const preferences = model && provider.preferences[model.modelId]
  return {
    id: model?.modelId ?? "", name: model?.displayName ?? "",
    context: model?.capabilities.contextWindow?.toString() ?? "", output: model?.capabilities.maxOutputTokens?.toString() ?? "",
    tools: model?.capabilities.toolCalling ?? true, vision: model?.capabilities.vision ?? false,
    reasoning: model?.capabilities.reasoning ?? false, efforts: model?.metadata?.reasoning?.effortOptions?.join(", ") ?? "",
    effort: preferences?.reasoningEffort ?? "", tier: preferences?.serviceTier ?? "",
    endpoint: entry?.endpointOverride ?? "", alias: entry?.alias ?? "",
  }
}

function modelFromDraft(draft: Draft): ModelEntry {
  return {
    modelId: draft.id, displayName: draft.name, status: "active",
    capabilities: {
      modalities: ["text"], toolCalling: draft.tools, streaming: true, structuredOutput: true, vision: draft.vision,
      audio: false, reasoning: draft.reasoning, ...(draft.context ? { contextWindow: Number(draft.context) } : {}),
      ...(draft.output ? { maxOutputTokens: Number(draft.output) } : {}),
    },
    ...(draft.reasoning ? { metadata: { reasoning: { supported: true, effortOptions: effortsFor(draft.efforts) } } } : {}),
  }
}

function modelEdit(draft: Draft, initial: Draft): ModelEdits {
  return {
    ...(draft.name !== initial.name ? { displayName: draft.name } : {}),
    ...(draft.context && draft.context !== initial.context ? { contextWindow: Number(draft.context) } : {}),
    ...(draft.output && draft.output !== initial.output ? { maxOutputTokens: Number(draft.output) } : {}),
    ...(draft.tools !== initial.tools ? { toolCalling: draft.tools } : {}),
    ...(draft.vision !== initial.vision ? { vision: draft.vision } : {}),
    ...(draft.reasoning !== initial.reasoning || draft.efforts !== initial.efforts
      ? { reasoning: { supported: draft.reasoning, effortOptions: effortsFor(draft.efforts) } } : {}),
    ...(draft.endpoint !== initial.endpoint ? { endpoint: field(draft.endpoint) } : {}),
    ...(draft.alias !== initial.alias ? { alias: field(draft.alias) } : {}),
  }
}

function identification(model: ManagementModel, providerId: string, text: Text) {
  const found = model.model.metadata?.identification
  if (!found) return text("Catalog identification unavailable", "没有目录识别结果")
  if (found.rule === "ambiguous") return `${text("Ambiguous match", "匹配不明确")} · ${found.candidates.map(({ providerId, modelId }) => `${providerId}/${modelId}`).join(", ")}`
  const matched = `${model.model.metadata?.matchedProviderId ?? providerId}/${model.model.metadata?.matchedModelId ?? model.model.modelId}`
  return `${found.rule === "merge" ? text("Inferred from", "推断自") : text("Matched", "匹配到")} ${matched}`
}

export function ProviderModels({ providerId, online, refreshCount, text, onDirty, onVersion }: {
  providerId: string; online: boolean; refreshCount: number; text: Text; onDirty: (dirty: boolean) => void; onVersion: (version: number) => void
}) {
  const [catalog, setCatalog] = useState<Selection>()
  const [query, setQuery] = useState("")
  const [isLoading, setIsLoading] = useState(false)
  const [busy, setBusy] = useState(false)
  const [editor, setEditor] = useState<Editor>()
  const [error, setError] = useState("")
  const [saved, setSaved] = useState("")
  const [conflict, setConflict] = useState(false)
  const request = useRef(0)
  const provider = catalog?.provider
  const dirty = editor !== undefined && JSON.stringify(editor.draft) !== JSON.stringify(editor.initial)
  const unavailable = busy || !online
  const can = (method: string) => !unavailable && connection.supports(method)

  useLayoutEffect(() => {
    onDirty(dirty)
    return () => onDirty(false)
  }, [dirty, onDirty])

  async function load(more = false) {
    if (!online || !connection.supports("provider.management")) return false
    const id = ++request.current
    setIsLoading(true)
    setError("")
    try {
      const response = await connection.client.call("provider.management", {
        providerId, page: { query: query.trim(), limit: 32, ...(more && catalog?.next ? { after: catalog.next } : {}) },
      }, readManagement)
      if (id !== request.current) return false
      const row = response.providers.find((item) => item.providerId === providerId)
      if (!row) throw new Error(response.errors.find((failure) => failure.providerId === providerId)?.message
        ?? text("Model catalog unavailable.", "模型目录不可用。"))
      if (more) {
        setCatalog((previous) => ({
          next: response.next,
          provider: {
            ...row,
            models: [...new Map([...(previous?.provider.models ?? []), ...(row.models ?? [])].map((model) => [model.model.modelId, model])).values()],
            preferences: { ...previous?.provider.preferences, ...row.preferences },
          },
        }))
      } else setCatalog({ next: response.next, provider: row })
      return true
    } catch (cause) {
      if (id === request.current) setError(cause instanceof Error ? cause.message : String(cause))
      return false
    } finally {
      if (id === request.current) setIsLoading(false)
    }
  }

  useEffect(() => {
    if (!online || dirty) return
    const timer = window.setTimeout(() => void load(), query ? 150 : 0)
    return () => {
      clearTimeout(timer)
      request.current++
    }
  }, [online, query, refreshCount])

  async function openEditor(model?: ManagementModel) {
    if (!provider || provider.version === null || unavailable) return
    setBusy(true)
    setError("")
    setSaved("")
    setConflict(false)
    try {
      let source = provider
      let version = provider.version
      let original = model
      if (model) {
        const result = await connection.client.call("provider.management", { providerId, modelId: model.model.modelId }, readManagement)
        const detail = result.providers.find((row) => row.providerId === providerId)
        original = detail?.models?.find((entry) => entry.model.modelId === model.model.modelId)
        if (!detail || !original || detail.version === null) throw new Error(result.errors[0]?.message
          ?? text("Model unavailable.", "模型不可用。"))
        source = detail
        version = detail.version
      }
      const initial = modelDraft(original, source)
      setEditor({ initial, draft: initial, original, version })
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  function edit(values: Partial<Draft>) {
    setEditor((current) => current && { ...current, draft: { ...current.draft, ...values } })
  }

  function close() {
    if (dirty && !window.confirm(text("Discard unsaved model changes?", "放弃未保存的模型更改？"))) return
    setEditor(undefined)
    setConflict(false)
    setError("")
  }

  async function write(method: string, version: number, params: Record<string, unknown>) {
    const result = await connection.client.call(method, {
      providerId, expectedVersion: version, ...(method !== "provider.model.set_enabled" ? { reply: "summary" } : {}), ...params,
    }, readModelWrite)
    onVersion(result.version)
    return result
  }

  async function refreshRow(modelId: string, version: number, deleted = false) {
    if (query.trim() || !catalog) return load()
    try {
      let fresh: ManagementProvider | undefined
      if (!deleted) {
        const result = await connection.client.call("provider.management", { providerId, modelId }, readManagement)
        fresh = result.providers.find((row) => row.providerId === providerId)
        if (!fresh) throw new Error(result.errors[0]?.message ?? text("Model unavailable.", "模型不可用。"))
      }
      const changed = fresh?.models?.find((entry) => entry.model.modelId === modelId)
      const models = new Map((catalog.provider.models ?? []).map((entry) => [entry.model.modelId, entry]))
      if (changed) models.set(modelId, changed)
      else models.delete(modelId)
      const preferences = { ...catalog.provider.preferences }
      delete preferences[modelId]
      setCatalog({
        next: catalog.next,
        provider: {
          ...(fresh ?? catalog.provider), version: fresh?.version ?? version,
          models: [...models.values()], preferences: { ...preferences, ...fresh?.preferences },
        },
      })
      return true
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
      return false
    }
  }

  function writeError(cause: unknown) {
    const stale = cause instanceof RpcError && cause.appError === "provider_conflict"
    setConflict(stale)
    setError(stale ? text("The provider changed. Reload before saving again. Your draft is still here.", "供应商已更改。请重新加载后再保存，草稿仍在。")
      : cause instanceof Error ? cause.message : String(cause))
  }

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!editor || !dirty || unavailable || conflict) return
    const { original, initial, draft } = editor
    const modelId = draft.id
    const changes = modelEdit(draft, initial)
    const preferences = {
      ...(draft.effort !== initial.effort ? { reasoningEffort: field(draft.effort) } : {}),
      ...(draft.tier !== initial.tier ? { serviceTier: field(draft.tier) } : {}),
    }
    setBusy(true)
    setError("")
    setSaved("")
    try {
      let version = editor.version
      if (!original) {
        version = (await write("provider.models.create", version, { model: modelFromDraft(draft) })).version
      } else if (Object.keys(changes).length > 0) {
        version = (await write("provider.models.edit", version, { modelId, edit: changes })).version
        setEditor({ ...editor, version, initial: { ...draft, effort: initial.effort, tier: initial.tier } })
      }
      if (original && Object.keys(preferences).length > 0) {
        version = (await write("provider.preferences.update", version, { modelId, patch: preferences })).version
      }
      setEditor(undefined)
      setSaved(text("Model saved.", "模型已保存。"))
      if (!(await refreshRow(modelId, version))) setConflict(true)
    } catch (cause) {
      writeError(cause)
    } finally {
      setBusy(false)
    }
  }

  async function action(model: ManagementModel, kind: "set_enabled" | "reset" | "delete") {
    const method = kind === "set_enabled" ? "provider.model.set_enabled" : `provider.models.${kind}`
    if (!can(method) || conflict || provider?.version === null || provider?.version === undefined) return
    if (kind !== "set_enabled" && !window.confirm(kind === "reset"
      ? text(`Reset overrides for ${model.model.displayName}?`, `重置 ${model.model.displayName} 的覆盖设置？`)
      : text(`Delete ${model.model.displayName}?`, `删除 ${model.model.displayName}？`))) return
    setBusy(true)
    setError("")
    setSaved("")
    try {
      const modelId = model.model.modelId
      const { version } = await write(method, provider.version, {
        modelId, ...(kind === "set_enabled" ? { enabled: !model.enabled } : {}),
      })
      setSaved(text("Model settings updated.", "模型设置已更新。"))
      if (!(await refreshRow(modelId, version, kind === "delete"))) setConflict(true)
    } catch (cause) {
      writeError(cause)
    } finally {
      setBusy(false)
    }
  }

  async function reload() {
    if (dirty && !window.confirm(text("Discard the draft and reload the model catalog?", "放弃草稿并重新加载模型目录？"))) return
    if (await load()) {
      setEditor(undefined)
      setConflict(false)
    }
  }

  return <section className="rw-provider-models">
    <h2>{text("Models", "模型")}</h2>
    {!connection.supports("provider.management") && <p role="status">{text("Model management is unavailable.", "模型管理不可用。")}</p>}
    {isLoading && <p role="status">{text("Loading models…", "正在加载模型…")}</p>}
    {saved && <p role="status">{saved}</p>}
    {error && <p role="alert" className="rw-error">{error}</p>}
    {(error || conflict) && <Button size="sm" variant="secondary" className="rw-settings-reload" isDisabled={unavailable || isLoading} onPress={() => void reload()}>
      {text("Reload model catalog", "重新加载模型目录")}
    </Button>}
    {editor && provider ? <ModelEditor editor={editor} provider={provider} text={text} dirty={dirty} disabled={unavailable || conflict}
      edit={edit} save={save} close={close} /> : <>
      <div className="rw-field-row">
        <label className="rw-setting-language">{text("Search models", "搜索模型")}
          <input type="search" value={query} onChange={(event) => setQuery(event.target.value)} />
        </label>
        {provider?.actions.includes("models.create") && <Button size="sm" isDisabled={conflict || isLoading || provider.version === null || !can("provider.models.create")}
          onPress={() => void openEditor()}>{text("Add model", "添加模型")}</Button>}
      </div>
      {provider?.models?.map((entry) => <section className="rw-settings-card" aria-label={entry.model.displayName} key={entry.model.modelId}>
        <h3>{entry.model.displayName}</h3>
        <p><code>{entry.model.modelId}</code> · {entry.source}</p>
        <p>{entry.unavailableReason ?? [
          entry.model.capabilities.contextWindow?.toLocaleString() ?? text("Context limit unknown", "上下文上限未知"),
          entry.model.capabilities.maxOutputTokens?.toLocaleString() ?? text("Output limit unknown", "输出上限未知"),
        ].join(" · ")}</p>
        {entry.missingLimits.length > 0 && <p>{text("Missing limits", "缺少限制")} · {entry.missingLimits.join(", ")}</p>}
        <p>{identification(entry, providerId, text)}</p>
        <div className="rw-setting-options">
          {entry.actions.includes("set_enabled") && <Button size="sm" variant="secondary" isDisabled={conflict || isLoading || !can("provider.model.set_enabled")}
            onPress={() => void action(entry, "set_enabled")}>{entry.enabled ? text("Disable model", "禁用模型") : text("Enable model", "启用模型")}</Button>}
          {entry.actions.includes("edit") && <Button size="sm" variant="secondary" isDisabled={conflict || isLoading || !can("provider.models.edit")}
            onPress={() => void openEditor(entry)}>{text("Edit model", "编辑模型")}</Button>}
          {entry.actions.includes("reset") && <Button size="sm" variant="secondary" isDisabled={conflict || isLoading || !can("provider.models.reset")}
            onPress={() => void action(entry, "reset")}>{text("Reset model", "重置模型")}</Button>}
          {entry.actions.includes("delete") && <Button size="sm" variant="danger-soft" isDisabled={conflict || isLoading || !can("provider.models.delete")}
            onPress={() => void action(entry, "delete")}>{text("Delete model", "删除模型")}</Button>}
        </div>
      </section>)}
      {catalog && !provider?.models?.length && <p>{text("No matching models.", "没有匹配的模型。")}</p>}
      {catalog?.next && <Button className="rw-settings-reload" size="sm" variant="secondary" isDisabled={unavailable || isLoading}
        onPress={() => void load(true)}>{text("Load more models", "加载更多模型")}</Button>}
    </>}
  </section>
}

function ModelEditor({ editor, provider, text, dirty, disabled, edit, save, close }: {
  editor: Editor; provider: ManagementProvider; text: Text; dirty: boolean; disabled: boolean
  edit: (values: Partial<Draft>) => void; save: (event: FormEvent<HTMLFormElement>) => void; close: () => void
}) {
  const { draft, original } = editor
  const allowedEfforts = original?.model.metadata?.reasoning?.effortOptions ?? []
  const allowedTiers = original?.model.metadata?.serviceTier?.options ?? []
  const preferences = !!original && provider.actions.includes("preferences.update") && connection.supports("provider.preferences.update")
  return <form className="rw-settings-card rw-settings-fields" onSubmit={save}>
    <h3>{original ? text("Edit model", "编辑模型") : text("Add model", "添加模型")}</h3>
    <fieldset className="rw-mcp-fields" disabled={disabled}>
      <label>{text("Model ID", "模型 ID")}<input required value={draft.id} readOnly={!!original} onChange={(event) => edit({ id: event.target.value })} /></label>
      <label>{text("Model name", "模型名称")}<input required value={draft.name} onChange={(event) => edit({ name: event.target.value })} /></label>
      <label>{text("Context window", "上下文窗口")}
        <input type="number" min="1" step="1" required={editor.initial.context !== ""} value={draft.context} onChange={(event) => edit({ context: event.target.value })} />
      </label>
      <label>{text("Max output tokens", "最大输出 Token")}
        <input type="number" min="1" step="1" required={editor.initial.output !== ""} value={draft.output} onChange={(event) => edit({ output: event.target.value })} />
      </label>
      <div className="rw-setting-options">
        {([
          ["tools", text("Tool calling", "工具调用")], ["vision", text("Vision", "视觉")], ["reasoning", text("Reasoning", "推理")],
        ] as const).map(([key, label]) => <label className="rw-recording-toggle" key={key}>
          <input type="checkbox" checked={draft[key]} onChange={(event) => edit({ [key]: event.target.checked })} />{label}
        </label>)}
      </div>
      {draft.reasoning && <label>{text("Available reasoning efforts", "可用推理强度")}
        <input value={draft.efforts} placeholder="low, medium, high" onChange={(event) => edit({ efforts: event.target.value })} />
      </label>}
      {original && provider.connection?.driverKind === "new-api-router" && <div className="rw-select-field">{text("Endpoint override", "端点覆盖")}
        <FieldSelect label={text("Endpoint override", "端点覆盖")} value={draft.endpoint}
          onChange={(selectedValue) => edit({ endpoint: selectedValue })} options={[
            { value: "", label: text("Inherit", "继承") },
            ...(draft.endpoint && !endpoints.includes(draft.endpoint) ? [{ value: draft.endpoint, label: draft.endpoint }] : []),
            ...endpoints.map((kind) => ({ value: kind, label: kind })),
          ]} />
      </div>}
      {original && <label>{text("Model alias", "模型别名")}<input value={draft.alias} onChange={(event) => edit({ alias: event.target.value })} /></label>}
      {preferences && allowedEfforts.length > 0 && <div className="rw-select-field">{text("Preferred effort", "首选推理强度")}
        <FieldSelect label={text("Preferred effort", "首选推理强度")} value={draft.effort}
          onChange={(selectedValue) => edit({ effort: selectedValue })} options={[
            { value: "", label: text("Inherit", "继承") },
            ...allowedEfforts.map((value) => ({ value: value, label: value })),
          ]} />
      </div>}
      {preferences && allowedTiers.length > 0 && <div className="rw-select-field">{text("Preferred service tier", "首选服务等级")}
        <FieldSelect label={text("Preferred service tier", "首选服务等级")} value={draft.tier}
          onChange={(selectedValue) => edit({ tier: selectedValue })} options={[
            { value: "", label: text("Inherit", "继承") },
            ...allowedTiers.map((value) => ({ value: value, label: value })),
          ]} />
      </div>}
    </fieldset>
    <div className="rw-setting-options">
      <Button size="sm" type="button" variant="secondary" onPress={close}>{text("Cancel", "取消")}</Button>
      <Button size="sm" type="submit" isDisabled={disabled || !dirty}>{text("Save model", "保存模型")}</Button>
    </div>
  </form>
}
