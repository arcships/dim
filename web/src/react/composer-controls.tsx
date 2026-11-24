import { Button, Popover } from "@heroui/react"
import { Boxes, Check, ChevronsUpDown, Search, Shield } from "lucide-react"
import { useCallback, useEffect, useImperativeHandle, useRef, useState, type Ref } from "react"
import { translate } from "./i18n"
import type { ModelId, PermissionPolicy, PermissionPreset } from "../rpc/protocol"
import { useModels } from "../stores/models"
import { useStoreValue } from "./stores"

const presets: PermissionPreset[] = ["read-only", "workspace-write", "full-access"]

export type PickerHandle = { open: () => void }

export function ModelPicker({ model, disabled, choose, follow, label = translate("composer.command.models"), placeholder = translate("composer.notConfigured"), ref }: {
  model: ModelId | undefined
  disabled: boolean
  choose: (value: ModelId) => void | Promise<void>
  follow?: () => void
  label?: string
  placeholder?: string
  ref?: Ref<PickerHandle>
}) {
  const [models] = useState(useModels)
  const catalog = useStoreValue(useCallback(() => ({
    providers: models.providers,
    error: models.error,
    loading: models.loading,
    next: models.next,
  }), [models]))
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState("")
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState("")
  const results = useRef<HTMLDivElement>(null)
  const search = useRef<HTMLInputElement>(null)
  const [browsing, setBrowsing] = useState<ModelId>()
  const target = browsing ?? model
  const preview = catalog.providers?.find(p => p.providerId === target?.providerId)?.models.find(row => row.id === target?.modelId)
    ?? catalog.providers?.[0]?.models[0]
  const modelName = model === undefined ? placeholder : models.nameOf(model)
  useImperativeHandle(ref, () => ({ open: () => setOpen(true) }), [])

  useEffect(() => {
    if (!open) return
    const timer = setTimeout(() => { void models.load({ query: query.trim() }) }, query === "" ? 0 : 180)
    return () => {
      clearTimeout(timer)
      models.clear()
    }
  }, [open, query, models])

  function changeOpen(next: boolean) {
    setOpen(next)
    if (!next) { setQuery(""); setError(""); setBrowsing(undefined) }
  }

  async function select(value: ModelId) {
    if (saving) return
    setSaving(true)
    setError("")
    try {
      await choose(value)
      changeOpen(false)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setSaving(false)
    }
  }

  return <Popover isOpen={open} onOpenChange={changeOpen}>
    <Button size="sm" variant="tertiary" className="rw-model rw-model-trigger" isDisabled={disabled} aria-label={`${label}: ${modelName}`}>
      <Boxes size={15} aria-hidden="true" /><span>{modelName}</span><ChevronsUpDown size={13} aria-hidden="true" />
    </Button>
    <Popover.Content placement="top start" className="rw-controls-menu rw-model-panel">
      <Popover.Dialog aria-label={label}>
        <label className="rw-model-search">
          <Search size={16} aria-hidden="true" />
          <input ref={search} autoFocus type="search" aria-label={translate("composer.searchModels")} placeholder={translate("composer.searchModels")} value={query}
            onChange={(event) => { setQuery(event.target.value); setBrowsing(undefined) }} onKeyDown={(event) => {
              if (event.key !== "ArrowDown" || event.nativeEvent.isComposing) return
              const first = results.current?.querySelector<HTMLButtonElement>("button:enabled")
              if (first) { event.preventDefault(); first.focus() }
            }} />
        </label>
        {follow && <Button size="sm" variant="tertiary" onPress={() => { follow(); changeOpen(false) }}>{translate("composer.followDefaults")}</Button>}
        {(catalog.error || error) && <p className="rw-error" role="alert">{error || catalog.error}</p>}
        {catalog.error && <Button size="sm" variant="tertiary" onPress={() => { void models.load({ query: query.trim() }) }}>{translate("composer.modelRetry")}</Button>}
        {catalog.loading && <p className="rw-muted" role="status">{translate("composer.modelLoading")}</p>}
        {catalog.providers?.length === 0 && !catalog.loading && <p className="rw-muted">{translate("composer.noMatches")}</p>}
        <div className="rw-control-options" ref={results} onKeyDown={(event) => {
          if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return
          const rows = [...event.currentTarget.querySelectorAll<HTMLButtonElement>("button:enabled")]
          const index = rows.indexOf(event.target as HTMLButtonElement)
          if (index < 0) return
          event.preventDefault()
          if (event.key === "ArrowUp" && index === 0) search.current?.focus()
          else rows[index + (event.key === "ArrowDown" ? 1 : -1)]?.focus()
        }}>
          {catalog.providers?.map((provider) => <div key={provider.providerId} className="rw-model-provider">
            <h3>{provider.name}</h3>
            {provider.models.map((row) => {
              const selected = model?.providerId === provider.providerId && model.modelId === row.id
              const id = { providerId: provider.providerId, modelId: row.id }
              return <Button key={row.id} size="sm" variant="tertiary" data-preview={preview === row || undefined}
                aria-pressed={selected} isDisabled={saving} onPointerMove={() => { if (preview !== row) setBrowsing(id) }} onFocus={() => setBrowsing(id)} onPress={() => { void select(id) }}>
                <Boxes size={15} aria-hidden="true" /><span>{row.name}</span>{selected && <Check size={14} aria-hidden="true" />}
              </Button>
            })}
          </div>)}
        </div>
        {catalog.next && <Button size="sm" variant="tertiary" isDisabled={catalog.loading} onPress={() => { void models.loadMore() }}>{translate("composer.loadMoreModels")}</Button>}
        {preview && <footer className="rw-model-details">
          <div>{preview.name !== preview.id && <>{preview.name} · </>}<code>{preview.id}</code></div>
          {!!preview.reasoningEfforts?.length && <span>{translate("composer.supportedEfforts")}: {preview.reasoningEfforts.join(" · ")}</span>}
        </footer>}
      </Popover.Dialog>
    </Popover.Content>
  </Popover>
}

export function PermissionPicker({ policy, disabled, choose, ref }: {
  policy: PermissionPolicy
  disabled: boolean
  choose: (value: PermissionPreset) => Promise<void>
  ref?: Ref<PickerHandle>
}) {
  const [open, setOpen] = useState(false)
  const [confirming, setConfirming] = useState(false)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState("")
  useImperativeHandle(ref, () => ({ open: () => setOpen(true) }), [])

  async function select(value: PermissionPreset) {
    if (saving || value === policy.preset) return
    setSaving(true)
    setError("")
    try {
      await choose(value)
      setOpen(false)
      setConfirming(false)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setSaving(false)
    }
  }

  return <Popover isOpen={open} onOpenChange={(next) => { setOpen(next); if (!next) setConfirming(false) }}>
    <Button size="sm" variant="tertiary" isDisabled={disabled} aria-label={translate("composer.permission.label")}>
      <Shield size={15} /> {translate(`composer.permission.preset.${policy.preset}`)}
    </Button>
    <Popover.Content placement="top start" className="rw-controls-menu">
      <Popover.Dialog aria-label={translate("composer.permission.label")}>
        {confirming
          ? <>
            <p>{translate("composer.permission.fullAccessConfirm")}</p>
            <div className="rw-control-actions">
              <Button size="sm" variant="tertiary" isDisabled={saving} onPress={() => setConfirming(false)}>{translate("common.cancel")}</Button>
              <Button size="sm" isPending={saving} onPress={() => { void select("full-access") }}>{translate("composer.permission.preset.full-access")}</Button>
            </div>
          </>
          : <div className="rw-control-options">
            {presets.map((preset) => <Button key={preset} size="sm" variant={preset === policy.preset ? "secondary" : "tertiary"}
              aria-pressed={preset === policy.preset} isDisabled={saving}
              onPress={() => { if (preset !== policy.preset && preset === "full-access") setConfirming(true); else void select(preset) }}>
              <span>{translate(`composer.permission.preset.${preset}`)}<small>{translate(`composer.permission.description.${preset}`)}</small></span>
              {preset === policy.preset && <Check size={14} />}
            </Button>)}
          </div>}
        {policy.allowedTools.length > 0 && <p className="rw-muted">{translate("composer.permission.allowedTools")} · {policy.allowedTools.join(", ")}</p>}
        {policy.disallowedTools.length > 0 && <p className="rw-muted">{translate("composer.permission.disallowedTools")} · {policy.disallowedTools.join(", ")}</p>}
        {error && <p className="rw-error" role="alert">{error}</p>}
      </Popover.Dialog>
    </Popover.Content>
  </Popover>
}
