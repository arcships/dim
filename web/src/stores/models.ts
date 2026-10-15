import { computed, ref } from "vue"
import { defineStore } from "pinia"
import { readProviderCatalog, readProviderList, readProviderSelection } from "../rpc/read"
import type { ModelId, ProviderModel } from "../rpc/protocol"
import { useConnectionStore } from "./connection"

export type ProviderModels = { providerId: string; name: string; models: ProviderModel[] }

/**
 * The selectable models (`provider.list` + `provider.catalog`, read-only on the
 * web entry). Loaded on first use; a provider whose catalog fails is left out.
 */
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

  async function loadProviders(): Promise<void> {
    const client = useConnectionStore().client
    error.value = ""
    try {
      const list = await client.call("provider.list", {}, readProviderList)
      const usable = list.filter((provider) => provider.enabled && provider.credentialConfigured)
      const catalogs = await Promise.all(usable.map((provider) => client.call("provider.catalog", { providerId: provider.providerId }, readProviderCatalog).catch(() => undefined)))
      providers.value = usable.flatMap((provider, index) => {
        const catalog = catalogs[index]
        if (catalog === undefined) return []
        const disabled = new Set(catalog.disabledModelIds)
        const models = catalog.models.filter((model) => model.unavailableReason === undefined && !disabled.has(model.id))
        return models.length === 0 ? [] : [{ providerId: provider.providerId, name: provider.displayName, models }]
      })
    } catch (cause) {
      error.value = cause instanceof Error ? cause.message : String(cause)
    }
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

  return { providers, error, available, load, defaultFor, nameOf }
})
