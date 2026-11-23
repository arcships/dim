import { Button, Popover } from "@heroui/react"
import { Check, ChevronDown, Shield } from "lucide-react"
import { useCallback, useEffect, useImperativeHandle, useState, type Ref } from "react"
import { translate } from "./i18n"
import type { ModelId, PermissionPolicy, PermissionPreset } from "../rpc/protocol"
import { useModels } from "../stores/models"
import { useStoreValue } from "./stores"

const presets: PermissionPreset[] = ["read-only", "workspace-write", "full-access"]

export type PickerHandle = { open: () => void }

export function ModelPicker({ model, disabled, choose, follow, label = "Choose model", placeholder = "Default model", ref }: {
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
  useImperativeHandle(ref, () => ({ open: () => setOpen(true) }), [])

  useEffect(() => {
    if (!open) return
    const timer = setTimeout(() => { void models.load({ query: query.trim() }) }, query === "" ? 0 : 180)
    return () => {
      clearTimeout(timer)
      models.clear()
    }
  }, [open, query, models])

  async function select(value: ModelId) {
    if (saving) return
    setSaving(true)
    setError("")
    try {
      await choose(value)
      setOpen(false)
      setQuery("")
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setSaving(false)
    }
  }

  return <Popover isOpen={open} onOpenChange={setOpen}>
    <Button size="sm" variant="tertiary" className="rw-model" isDisabled={disabled} aria-label={label}>
      <span>{model === undefined ? placeholder : models.nameOf(model)}</span><ChevronDown size={13} />
    </Button>
    <Popover.Content placement="top start" className="rw-controls-menu">
      <Popover.Dialog aria-label={label}>
        <label className="rw-model-search">
          <span>Search models</span>
          <input autoFocus value={query} onChange={(event) => setQuery(event.target.value)} />
        </label>
        {follow && <Button size="sm" variant="tertiary" onPress={() => { follow(); setOpen(false) }}>Follow workspace default</Button>}
        {(catalog.error || error) && <p className="rw-error" role="alert">{error || catalog.error}</p>}
        {catalog.error && <Button size="sm" variant="tertiary" onPress={() => { void models.load({ query: query.trim() }) }}>Retry model search</Button>}
        {catalog.loading && <p className="rw-muted" role="status">Loading models…</p>}
        {catalog.providers?.length === 0 && !catalog.loading && <p className="rw-muted">No matching models</p>}
        <div className="rw-control-options">
          {catalog.providers?.map((provider) => <div key={provider.providerId} className="rw-model-provider">
            <h3>{provider.name}</h3>
            {provider.models.map((row) => {
              const selected = model?.providerId === provider.providerId && model.modelId === row.id
              return <Button key={row.id} size="sm" variant={selected ? "secondary" : "tertiary"}
                aria-pressed={selected} isDisabled={saving} onPress={() => { void select({ providerId: provider.providerId, modelId: row.id }) }}>
                {row.name}{selected && <Check size={14} />}
              </Button>
            })}
          </div>)}
        </div>
        {catalog.next && <Button size="sm" variant="tertiary" isDisabled={catalog.loading} onPress={() => { void models.loadMore() }}>Load more models</Button>}
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
