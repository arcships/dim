import { computed, ref } from "vue"
import { defineStore } from "pinia"
import { readAccountProviders, readAck, readProviderCatalog, readProviderList, readProviderSelection } from "../rpc/read"
import type { ModelId, ProviderModel } from "../rpc/protocol"
import { useConnectionStore } from "./connection"

export type ProviderModels = { providerId: string; name: string; models: ProviderModel[] }

/** Selectable static and managed models, loaded through the web entry's read-only methods. */
export const useModelsStore = defineStore("models", () => {
  const providers = ref<ProviderModels[]>()
  const error = ref("")
  let loading: Promise<void> | undefined

  const available = computed(() => {
    const connection = useConnectionStore()
    return connection.supports("provider.list") && connection.supports("provider.catalog")
  })

  function load(): Promise<void> {
    loading ??= loadProviders().finally(() => { loading = undefined })
    return loading
  }

  async function refresh(providerId: string): Promise<void> {
    try {
      await useConnectionStore().client.call("provider.refresh", { providerId }, readAck)
    } finally {
      await loading
      await load()
    }
  }

  async function loadProviders(): Promise<void> {
    const connection = useConnectionStore()
    const client = connection.client
    error.value = ""
    const discovery = await Promise.allSettled([
      client.call("provider.list", {}, readProviderList).then((list) => list.filter((provider) => provider.enabled)),
      connection.supports("account.providers") ? client.call("account.providers", {}, readAccountProviders) : [],
    ])
    const candidates = discovery.flatMap((result) => {
      if (result.status === "rejected") {
        error.value = result.reason instanceof Error ? result.reason.message : String(result.reason)
        return []
      }
      return result.value
    })
    const usable = [...new Map(candidates.map((provider) => [provider.providerId, provider])).values()]
    const catalogs = await Promise.allSettled(usable.map((provider) => client.call("provider.catalog", { providerId: provider.providerId }, readProviderCatalog)))
    providers.value = usable.flatMap((provider, index) => {
      const result = catalogs[index]!
      if (result.status === "rejected") {
        error.value = result.reason instanceof Error ? result.reason.message : String(result.reason)
        return []
      }
      const catalog = result.value
      if (catalog === undefined) return []
      const disabled = new Set(catalog.disabledModelIds)
      const models = catalog.models.filter((model) => model.unavailableReason === undefined && !disabled.has(model.id))
      return models.length === 0 ? [] : [{ providerId: provider.providerId, name: provider.displayName, models }]
    })
  }

  /** The default model a new session in `cwd` would get, when the entry reports one. */
  async function defaultFor(cwd: string): Promise<ModelId | undefined> {
    if (!useConnectionStore().supports("provider.selection")) return undefined
    return useConnectionStore().client.call("provider.selection", { cwd }, readProviderSelection).catch(() => undefined)
  }

  function nameOf(model: ModelId): string {
    const provider = providers.value?.find((item) => item.providerId === model.providerId)
    return provider?.models.find((item) => item.id === model.modelId)?.name ?? model.modelId
  }

  return { providers, error, available, load, refresh, defaultFor, nameOf }
})
