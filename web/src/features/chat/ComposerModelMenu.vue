<script setup lang="ts">
import { computed, nextTick, onMounted, ref } from "vue"
import { useI18n } from "vue-i18n"
import { Check, LoaderCircle, Search } from "lucide-vue-next"
import type { ModelId } from "../../rpc/protocol"
import { useModelsStore } from "../../stores/models"

/**
 * Provider and model picker: a popover with search, a provider pane and a model
 * pane. When the model cannot be changed (no provider methods, or no
 * `session.model.update` on this connection) it only names the current model.
 */
type Option = { providerId: string; providerName: string; modelId: string; name: string }

const props = defineProps<{ model: ModelId | undefined; selectable: boolean; disabled: boolean }>()
const emit = defineEmits<{ select: [model: ModelId] }>()
const { t } = useI18n()
const models = useModelsStore()
const details = ref<HTMLDetailsElement>()
const searchInput = ref<HTMLInputElement>()
const query = ref("")
const selectedProvider = ref<string>()
const activeIndex = ref(0)

const providers = computed(() => models.providers ?? [])
const allOptions = computed<Option[]>(() => providers.value.flatMap((provider) => provider.models.map((model) => ({
  providerId: provider.providerId,
  providerName: provider.name,
  modelId: model.id,
  name: model.name,
}))))
const searching = computed(() => query.value.trim() !== "")
const paneOptions = computed(() => {
  if (!searching.value) return allOptions.value.filter((option) => option.providerId === selectedProvider.value)
  const needle = query.value.trim().toLocaleLowerCase()
  return allOptions.value.filter((option) => [option.name, option.modelId, option.providerName].some((value) => value.toLocaleLowerCase().includes(needle)))
})
const triggerLabel = computed(() => props.model === undefined ? t("composer.notConfigured") : models.nameOf(props.model))

// The trigger names the model from the catalog once it is loaded.
onMounted(() => {
  if (props.selectable && models.available && models.providers === undefined) void models.load()
})

function isCurrent(option: Option): boolean {
  return option.providerId === props.model?.providerId && option.modelId === props.model.modelId
}

function initial(name: string): string {
  return name.trim().charAt(0).toLocaleUpperCase()
}

async function onToggle(): Promise<void> {
  if (details.value?.open !== true) return
  if (models.providers === undefined) await models.load()
  query.value = ""
  selectedProvider.value = props.model?.providerId ?? providers.value[0]?.providerId
  activeIndex.value = Math.max(0, paneOptions.value.findIndex(isCurrent))
  await nextTick()
  if (!matchMedia("(pointer: coarse)").matches) searchInput.value?.focus()
}

function select(option: Option): void {
  if (details.value) details.value.open = false
  if (!isCurrent(option)) emit("select", { providerId: option.providerId, modelId: option.modelId })
}

function onSearchKey(event: KeyboardEvent): void {
  if (event.key === "ArrowDown") {
    event.preventDefault()
    activeIndex.value = Math.min(paneOptions.value.length - 1, activeIndex.value + 1)
  } else if (event.key === "ArrowUp") {
    event.preventDefault()
    activeIndex.value = Math.max(0, activeIndex.value - 1)
  } else if (event.key === "Enter") {
    event.preventDefault()
    const option = paneOptions.value[activeIndex.value]
    if (option !== undefined) select(option)
  }
}
</script>

<template>
  <span v-if="!selectable || !models.available" class="ui-button model-trigger" data-variant="ghost" data-size="sm" aria-disabled="true">
    <span class="model-trigger-label">{{ triggerLabel }}</span>
  </span>
  <details v-else ref="details" class="composer-popover model-menu" :class="{ 'is-disabled': disabled }" @toggle="onToggle">
    <summary class="ui-button model-trigger" data-variant="ghost" data-size="sm" v-tooltip="model ? `${triggerLabel} · ${model.providerId}` : undefined">
      <span v-if="model" class="provider-badge" aria-hidden="true">{{ initial(model.providerId) }}</span>
      <span class="model-trigger-label">{{ triggerLabel }}</span>
    </summary>
    <div class="composer-popover-content is-end ui-popover model-content" data-width="xl">
      <label class="ui-menu-search model-search">
        <Search aria-hidden="true" />
        <input ref="searchInput" v-model="query" type="text" :placeholder="t('composer.searchModels')" :aria-label="t('composer.searchModels')" @keydown="onSearchKey" @input="activeIndex = 0">
      </label>
      <p v-if="models.providers === undefined && !models.error" class="menu-empty"><LoaderCircle class="is-spinning" aria-hidden="true" />{{ t("composer.modelLoading") }}</p>
      <p v-else-if="models.error" class="menu-empty">
        <button type="button" class="ui-button" data-variant="danger-ghost" data-size="sm" @click="models.load()">{{ t("composer.modelLoadRetry") }}</button>
      </p>
      <div v-else class="model-panes">
        <div v-if="!searching" class="provider-pane">
          <button v-for="provider in providers" :key="provider.providerId" type="button" class="ui-menu-item" :aria-current="selectedProvider === provider.providerId" @click="selectedProvider = provider.providerId; activeIndex = 0">
            <span class="provider-badge" aria-hidden="true">{{ initial(provider.name) }}</span><span>{{ provider.name }}</span>
          </button>
        </div>
        <div class="model-pane" role="listbox" :aria-label="t('composer.searchModels')">
          <p v-if="paneOptions.length === 0" class="menu-empty">{{ t("composer.noModels") }}</p>
          <button
            v-for="(option, index) in paneOptions"
            :key="`${option.providerId}:${option.modelId}`"
            type="button"
            role="option"
            class="ui-menu-item"
            :aria-selected="isCurrent(option)"
            :data-highlighted="index === activeIndex ? '' : undefined"
            :title="`${option.name} · ${option.providerName}`"
            @mouseenter="activeIndex = index"
            @click="select(option)"
          >
            <span class="model-check"><Check v-if="isCurrent(option)" aria-hidden="true" /></span>
            <span v-if="searching" class="provider-badge" aria-hidden="true">{{ initial(option.providerName) }}</span>
            <span class="model-name">{{ option.name }}</span>
          </button>
        </div>
      </div>
    </div>
  </details>
</template>

<style scoped>
.model-trigger { max-width: min(260px, 32vw); }
.model-trigger-label { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
/* Provider initial: a 16px glyph tile on the xs control radius. */
.provider-badge { display: grid; width: 16px; height: 16px; flex: none; place-items: center; border-radius: var(--control-xs-radius); background: var(--muted); color: var(--foreground); font-size: var(--text-xs); font-weight: var(--weight-medium); line-height: 1; }
.model-name { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.model-content { display: flex; flex-direction: column; overflow: hidden; }
.model-panes { display: flex; height: 240px; min-height: 96px; margin-inline: calc(-1 * var(--space-1)); }
.provider-pane { display: flex; width: 132px; flex: none; flex-direction: column; gap: 2px; overflow-y: auto; padding: 0 var(--space-1); border-right: 1px solid var(--border); }
.model-pane { display: flex; min-width: 0; flex: 1; flex-direction: column; gap: 2px; overflow-y: auto; padding: 0 var(--space-1); }
.model-check { display: flex; width: 14px; height: 14px; flex: none; align-items: center; justify-content: center; color: var(--primary-ink); }
.model-check svg { width: 14px; height: 14px; }
</style>
