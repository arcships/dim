import { computed, getCurrentScope, onScopeDispose, reactive, ref } from "vue"
import { readAck, readModelPage, readProviderSelection } from "../rpc/read"
import type { ModelId, ProviderModel } from "../rpc/protocol"
import { useConnectionStore } from "./connection"

export type ProviderModels = { providerId: string; name: string; models: ProviderModel[] }
export type ModelQuery = { providerId?: string; modelId?: string; query?: string }

/** Results belong to the view that requested them and are released with it. */
export function useModels() {
  const providers = ref<ProviderModels[]>()
  const error = ref("")
  const next = ref<ModelId>()
  const loading = ref(false)
  let generation = 0
  let current: ModelQuery = {}
  const available = computed(() => useConnectionStore().supports("provider.models.query"))

  async function load(input: ModelQuery = {}, more = false): Promise<void> {
    const request = ++generation
    current = input
    loading.value = true
    error.value = ""
    try {
      const page = await useConnectionStore().client.call("provider.models.query", { ...input, ...(more && next.value ? { after: next.value } : {}) }, readModelPage)
      if (request !== generation) return
      const grouped = new Map<string, ProviderModels>(more ? providers.value?.map((p) => [p.providerId, { ...p, models: [...p.models] }]) : [])
      for (const row of page.models) {
        if (row.disabled || row.model.unavailableReason !== undefined) continue
        let provider = grouped.get(row.providerId)
        if (!provider) { provider = { providerId: row.providerId, name: row.providerName, models: [] }; grouped.set(row.providerId, provider) }
        provider.models.push(row.model)
      }
      providers.value = [...grouped.values()]
      next.value = page.next
    } catch (cause) {
      if (request === generation) error.value = cause instanceof Error ? cause.message : String(cause)
    } finally {
      if (request === generation) loading.value = false
    }
  }

  function clear(): void {
    generation++
    providers.value = undefined
    next.value = undefined
    loading.value = false
    error.value = ""
  }

  async function refresh(providerId: string): Promise<void> {
    await useConnectionStore().client.call("provider.refresh", { providerId }, readAck)
    if (providers.value !== undefined || loading.value) await load(current)
  }

  async function defaultFor(cwd: string): Promise<ModelId | undefined> {
    if (!useConnectionStore().supports("provider.selection")) return undefined
    return useConnectionStore().client.call("provider.selection", { cwd }, readProviderSelection).catch(() => undefined)
  }

  function nameOf(model: ModelId): string {
    return providers.value?.find((item) => item.providerId === model.providerId)?.models.find((item) => item.id === model.modelId)?.name ?? model.modelId
  }

  if (getCurrentScope()) onScopeDispose(clear)

  return reactive({ providers, error, available, loading, next, load, loadMore: () => load(current, true), clear, refresh, defaultFor, nameOf })
}
