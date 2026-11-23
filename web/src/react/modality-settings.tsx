import { FieldSelect } from "./field-select"
import { Button } from "@heroui/react"
import { RefreshCw } from "lucide-react"
import { useCallback, useEffect, useLayoutEffect, useState, type FormEvent } from "react"
import { RpcError } from "../rpc/client"
import { readModalitySnapshot, type ModalitySnapshot } from "../rpc/general-settings"
import type { ModelId } from "../rpc/protocol"
import { useModels } from "../stores/models"
import { connection, useStoreValue } from "./stores"

const readPhase = () => connection.phase
const operationTitles: Record<string, readonly [string, string]> = {
  generate: ["Image generation", "图像生成"],
  edit: ["Image editing", "图像编辑"],
  enhance: ["Image enhancement", "图像增强"],
  understand: ["Image understanding", "图像理解"],
}
const modelKey = (model: ModelId | undefined | null) => model ? JSON.stringify([model.providerId, model.modelId]) : ""

export function ModalitySettings({ text, onDirty }: {
  text: (en: string, zh: string) => string
  onDirty: (dirty: boolean) => void
}) {
  const phase = useStoreValue(readPhase)
  const [models] = useState(useModels)
  const providers = useStoreValue(useCallback(() => models.providers, [models]))
  const [snapshot, setSnapshot] = useState<ModalitySnapshot>()
  const [draft, setDraft] = useState<ReadonlyMap<string, ModelId | null>>(new Map())
  const [busy, setBusy] = useState(false)
  const [conflict, setConflict] = useState(false)
  const [error, setError] = useState("")
  const [saved, setSaved] = useState(false)
  const connected = phase === "connected"
  const dirty = draft.size > 0
  const category = snapshot?.categories.find((item) => item.category === "image")
  const disabled = busy || conflict || !connected || !connection.supports("modality.configure")

  useLayoutEffect(() => {
    onDirty(dirty)
    return () => onDirty(false)
  }, [dirty, onDirty])

  useEffect(() => () => models.clear(), [models])

  function label(model: ModelId) {
    const provider = providers?.find(({ providerId }) => providerId === model.providerId)
    return `${provider?.name ?? model.providerId} / ${provider?.models.find(({ id }) => id === model.modelId)?.name ?? model.modelId}`
  }

  async function load() {
    if (busy || !connected) return
    if (dirty && !window.confirm(text("Discard unsaved defaults?", "放弃未保存的默认设置？"))) return
    setBusy(true)
    setError("")
    setSaved(false)
    try {
      if (!connection.supports("modality.capabilities")) throw new Error(text("Modality settings are unavailable on this server.", "当前服务器不支持多模态设置。"))
      const [next] = await Promise.all([
        connection.client.call("modality.capabilities", {}, readModalitySnapshot),
        models.available ? models.load() : undefined,
      ])
      if (!next.categories.some(({ category }) => category === "image")) {
        throw new Error(text("The server reports no image category for Modality.", "服务器未返回多模态的图像类别。"))
      }
      setSnapshot(next)
      setDraft(new Map())
      setConflict(false)
      if (models.error) setError(models.error)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  useEffect(() => {
    if (connected && !dirty) void load()
  }, [connected])

  function update(operation: string, model: ModelId | null, initial: ModelId | undefined) {
    setDraft((current) => {
      const next = new Map(current)
      if (modelKey(model) === modelKey(initial)) next.delete(operation)
      else next.set(operation, model)
      return next
    })
    setSaved(false)
  }

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!snapshot || !category || disabled || !dirty) return
    setBusy(true)
    setError("")
    setSaved(false)
    try {
      let revision = snapshot.revision
      for (const { operation } of category.operations) {
        const model = draft.get(operation)
        if (model === undefined) continue
        const next = await connection.client.call("modality.configure", {
          expectedRevision: revision,
          category: category.category,
          operation,
          model,
        }, readModalitySnapshot)
        setSnapshot(next)
        revision = next.revision
        setDraft((current) => {
          const remaining = new Map(current)
          remaining.delete(operation)
          return remaining
        })
      }
      setSaved(true)
    } catch (cause) {
      const stale = cause instanceof RpcError && cause.appError === "modality_conflict"
      setConflict(stale)
      setError(stale
        ? text("Modality defaults changed. Reload before saving again. Your draft is still here.", "多模态默认设置已变更。请重新加载后再保存，当前草稿已保留。")
        : cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  return <article className="rw-settings">
    <h1>{text("Modality", "多模态")}</h1>
    <p>{text("Choose a default model for each image operation, or use automatic resolution.", "为每个图像操作选择默认模型，也可使用自动解析。")}</p>
    <Button className="rw-settings-reload" size="sm" variant="secondary" isPending={busy} isDisabled={!connected} onPress={() => void load()}>
      <RefreshCw size={15} /> {text("Reload models", "重新加载模型")}
    </Button>
    {error && <p className="rw-error" role="alert">{error}</p>}
    {saved && !dirty && <p role="status">{text("Defaults saved.", "默认设置已保存。")}</p>}
    {category && <form className="rw-settings-form" onSubmit={save}>
      {category.reasons.map(({ dimension, detail }) => <p className="rw-muted" key={`${dimension}/${detail}`}>{detail}</p>)}
      {category.operations.map((row) => {
        const title = Object.hasOwn(operationTitles, row.operation) ? text(...operationTitles[row.operation]!) : row.operation
        const choice = draft.has(row.operation) ? draft.get(row.operation) : row.default
        const missingDefault = row.options.some((option) => modelKey(option) === modelKey(row.default)) ? undefined : row.default
        return <section className="rw-settings-card" key={row.operation}>
          <h2>{title}</h2>
          {row.reasons.map(({ dimension, detail }) => <p className="rw-muted" key={`${dimension}/${detail}`}>{detail}</p>)}
          {row.key !== undefined && <div className="rw-select-field">
            {text("Default model", "默认模型")}
            <FieldSelect label={`${title} · ${text("Default model", "默认模型")}`} value={modelKey(choice)} disabled={disabled || !row.available}
              onChange={(selectedValue) => {
                const value = selectedValue
                if (value === "") update(row.operation, null, row.default)
                else {
                  const option = row.options.find((option) => modelKey(option) === value)
                  if (option) update(row.operation, { providerId: option.providerId, modelId: option.modelId }, row.default)
                }
              }} options={[
                { value: "", label: text("Automatic", "自动") },
                ...(missingDefault ? [{ value: modelKey(missingDefault), label: `${label(missingDefault)} (${text("unavailable", "不可用")})`, disabled: true }] : []),
                ...row.options.map((option) => ({ value: modelKey(option), label: `${label(option)}${option.available ? "" : ` (${text("unavailable", "不可用")})`}`, disabled: !option.available })),
              ]} />
          </div>}
          {!row.default?.available && row.default?.reason && <p className="rw-muted">{row.default.reason.detail}</p>}
          <dl className="rw-modality-effective">
            <dt>{text("Currently effective", "当前生效")}</dt>
            <dd>{row.effective
              ? `${label(row.effective)} (${row.effective.source === "automatic" ? text("automatic", "自动") : text("configured", "已配置")})`
              : row.default ? text("Configured model unavailable", "已配置模型不可用") : text("No model available", "没有可用模型")}</dd>
          </dl>
          {row.operation === "understand" && <p>{text("Used by image_understand. The conversation keeps its own model.", "由 image_understand 使用。会话保留自己的模型。")}</p>}
        </section>
      })}
      <Button type="submit" isDisabled={disabled || !dirty}>{text("Save defaults", "保存默认设置")}</Button>
    </form>}
  </article>
}
