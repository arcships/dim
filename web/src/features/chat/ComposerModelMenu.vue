<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue"
import { useI18n } from "vue-i18n"
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
const open = ref(false)
const searchInput = ref<{ inputRef?: HTMLInputElement | null }>()
const triggerRef = ref<{ $el?: HTMLElement | null }>()
const query = ref("")
const selectedProvider = ref<string>()
const activeIndex = ref(0)

const providers = computed(() => models.providers ?? [])
const currentProvider = computed(() => providers.value.find((provider) => provider.providerId === selectedProvider.value)?.providerId
  ?? providers.value.find((provider) => provider.providerId === props.model?.providerId)?.providerId
  ?? providers.value[0]?.providerId)
const allOptions = computed<Option[]>(() => providers.value.flatMap((provider) => provider.models.map((model) => ({
  providerId: provider.providerId,
  providerName: provider.name,
  modelId: model.id,
  name: model.name,
}))))
const searching = computed(() => query.value.trim() !== "")
const paneOptions = computed(() => {
  if (!searching.value) return allOptions.value.filter((option) => option.providerId === currentProvider.value)
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

watch(open, async (isOpen) => {
  if (!isOpen) return
  if (models.providers === undefined) await models.load()
  query.value = ""
  selectedProvider.value = props.model?.providerId ?? providers.value[0]?.providerId
  activeIndex.value = Math.max(0, paneOptions.value.findIndex(isCurrent))
  await nextTick()
  if (!matchMedia("(pointer: coarse)").matches) searchInput.value?.inputRef?.focus()
})

function select(option: Option): void {
  open.value = false
  if (!isCurrent(option)) emit("select", { providerId: option.providerId, modelId: option.modelId })
}

function onSearchKey(event: KeyboardEvent): void {
  if (event.isComposing || event.keyCode === 229) return
  if (event.key === "Escape") {
    event.preventDefault()
    open.value = false
    triggerRef.value?.$el?.focus()
    return
  }
  if ((event.key === "ArrowLeft" || event.key === "ArrowRight") && !searching.value && providers.value.length > 1) {
    event.preventDefault()
    const step = event.key === "ArrowRight" ? 1 : -1
    const index = providers.value.findIndex((provider) => provider.providerId === currentProvider.value)
    const next = providers.value[(index + step + providers.value.length) % providers.value.length]
    if (next !== undefined) {
      selectedProvider.value = next.providerId
      activeIndex.value = 0
    }
  } else if (event.key === "ArrowDown") {
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

defineExpose({ open: () => {
  if (props.selectable && models.available && !props.disabled) open.value = true
} })
</script>

<template>
  <span v-if="!selectable || !models.available" class="model-trigger model-readonly" aria-disabled="true">
    <span class="model-trigger-label">{{ triggerLabel }}</span>
  </span>
  <UPopover v-else v-model:open="open" :content="{ side: 'top', align: 'end' }">
    <UTooltip :text="model ? `${triggerLabel} · ${model.providerId}` : undefined" :disabled="!model">
      <UButton ref="triggerRef" color="neutral" variant="ghost" size="sm" class="model-trigger" :disabled="disabled">
        <span v-if="model" class="provider-badge" aria-hidden="true">{{ initial(model.providerId) }}</span>
        <span class="model-trigger-label">{{ triggerLabel }}</span>
      </UButton>
    </UTooltip>
    <template #content>
      <div class="model-content">
        <UInput
          ref="searchInput"
          v-model="query"
          size="sm"
          icon="i-lucide-search"
          class="model-search"
          :placeholder="t('composer.searchModels')"
          :aria-label="t('composer.searchModels')"
          @keydown="onSearchKey"
          @update:model-value="activeIndex = 0"
        />
        <p v-if="models.providers === undefined && !models.error" class="menu-empty"><UIcon name="i-lucide-loader-circle" class="size-4 animate-spin" aria-hidden="true" />{{ t("composer.modelLoading") }}</p>
        <p v-else-if="models.error" class="menu-empty">
          <UButton color="error" variant="ghost" size="sm" @click="models.load()">{{ t("composer.modelLoadRetry") }}</UButton>
        </p>
        <div v-if="models.providers !== undefined && (!models.error || providers.length > 0)" class="model-panes">
          <div v-if="!searching" class="provider-pane" role="group" :aria-label="t('composer.providers')">
            <UButton
              v-for="provider in providers"
              :key="provider.providerId"
              color="neutral"
              :variant="currentProvider === provider.providerId ? 'soft' : 'ghost'"
              size="sm"
              block
              class="menu-option"
              :aria-current="currentProvider === provider.providerId"
              @click="selectedProvider = provider.providerId; activeIndex = 0"
            >
              <span class="provider-badge" aria-hidden="true">{{ initial(provider.name) }}</span>
              <span class="truncate">{{ provider.name }}</span>
            </UButton>
          </div>
          <div class="model-pane" role="listbox" :aria-label="t('composer.searchModels')">
            <p v-if="paneOptions.length === 0" class="menu-empty">{{ t("composer.noModels") }}</p>
            <UButton
              v-for="(option, index) in paneOptions"
              :key="`${option.providerId}:${option.modelId}`"
              color="neutral"
              :variant="index === activeIndex ? 'soft' : 'ghost'"
              size="sm"
              block
              role="option"
              class="menu-option"
              :aria-selected="isCurrent(option)"
              :title="`${option.name} · ${option.providerName}`"
              @mouseenter="activeIndex = index"
              @click="select(option)"
            >
              <span class="model-check"><UIcon v-if="isCurrent(option)" name="i-lucide-check" class="size-3.5" aria-hidden="true" /></span>
              <span v-if="searching" class="provider-badge" aria-hidden="true">{{ initial(option.providerName) }}</span>
              <span class="model-name">{{ option.name }}</span>
            </UButton>
          </div>
        </div>
      </div>
    </template>
  </UPopover>
</template>

<style scoped>
.model-trigger { max-width: min(260px, 32vw); }
.model-readonly { display: inline-flex; height: var(--control-sm-height); align-items: center; padding: 0 var(--space-2); color: var(--muted-foreground); font-size: var(--text-sm); }
.model-trigger-label { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
/* Provider initial: a 16px glyph tile on the xs control radius. */
.provider-badge { display: grid; width: 16px; height: 16px; flex: none; place-items: center; border-radius: var(--control-xs-radius); background: var(--muted); color: var(--foreground); font-size: var(--text-xs); font-weight: var(--weight-medium); line-height: 1; }
.model-name { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.model-content { display: flex; width: min(440px, calc(100vw - 24px)); flex-direction: column; gap: var(--space-2); overflow: hidden; padding: var(--space-2); }
.model-panes { display: flex; height: 240px; min-height: 96px; }
.provider-pane { display: flex; width: 132px; flex: none; flex-direction: column; gap: 2px; overflow-y: auto; padding-right: var(--space-2); border-right: 1px solid var(--border); }
.model-pane { display: flex; min-width: 0; flex: 1; flex-direction: column; gap: 2px; overflow-y: auto; padding-left: var(--space-2); }
.menu-option { justify-content: flex-start; text-align: left; }
.model-check { display: flex; width: 14px; height: 14px; flex: none; align-items: center; justify-content: center; color: var(--primary-ink); }
</style>
