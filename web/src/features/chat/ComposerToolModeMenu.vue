<script setup lang="ts">
import { ref, watch } from "vue"
import { useI18n } from "vue-i18n"
import type { ToolMode, ToolModeState } from "../../rpc/protocol"

/** Session tool mode; the picker lists every mode and marks one whose entry tool is absent. */
const props = defineProps<{ state: ToolModeState | undefined; available: ToolMode[] }>()
const emit = defineEmits<{ select: [mode: ToolMode]; close: [] }>()
const { t } = useI18n()
const popoverOpen = ref(false)
watch(popoverOpen, (isOpen, wasOpen) => { if (wasOpen && !isOpen) emit("close") })

const MODES: ToolMode[] = ["normal", "codemode", "shell"]

function unavailable(mode: ToolMode): boolean {
  return mode !== "normal" && !props.available.includes(mode)
}

function choose(mode: ToolMode): void {
  if (props.state === undefined || mode === props.state.mode) return
  emit("select", mode)
}

function open(): void {
  popoverOpen.value = true
}

defineExpose({ open })
</script>

<template>
  <UPopover v-model:open="popoverOpen" :content="{ side: 'top', align: 'start' }">
    <UButton color="neutral" variant="ghost" size="sm" icon="i-lucide-terminal" class="tool-mode-trigger" :aria-label="t('composer.toolMode.label')">
      <span class="tool-mode-trigger-label">{{ state ? t(`composer.toolMode.name.${state.mode}`) : t("composer.toolMode.label") }}</span>
    </UButton>
    <template #content>
      <div class="tool-mode-content">
        <fieldset class="tool-mode-group" :disabled="!state">
          <legend>{{ t("composer.toolMode.label") }}</legend>
          <UButton
            v-for="mode in MODES"
            :key="mode"
            color="neutral"
            :variant="mode === state?.mode ? 'soft' : 'ghost'"
            size="sm"
            block
            class="tool-mode-option"
            :aria-pressed="mode === state?.mode"
            @click="choose(mode)"
          >
            <span class="tool-mode-option-text">
              <span>{{ t(`composer.toolMode.name.${mode}`) }}</span>
              <small>{{ unavailable(mode) ? t("composer.toolMode.unavailable") : t(`composer.toolMode.description.${mode}`) }}</small>
            </span>
            <template #trailing>
              <UIcon v-if="mode === state?.mode" name="i-lucide-check" class="size-3.5 shrink-0 text-primary" aria-hidden="true" />
            </template>
          </UButton>
        </fieldset>
        <p v-if="state && state.effective !== state.mode" class="tool-mode-note">{{ t("composer.toolMode.fallback") }}</p>
      </div>
    </template>
  </UPopover>
</template>

<style scoped>
.tool-mode-trigger-label { max-width: 200px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tool-mode-content { display: grid; width: min(340px, calc(100vw - 24px)); gap: var(--space-2); max-height: 70dvh; overflow-y: auto; padding: var(--space-2); }
.tool-mode-group { display: grid; gap: 2px; min-width: 0; margin: 0; padding: 0; border: 0; }
.tool-mode-group legend { padding: var(--space-1) var(--space-2); font-size: var(--text-xs); font-weight: 600; }
.tool-mode-option { justify-content: flex-start; text-align: left; }
.tool-mode-option-text { display: grid; min-width: 0; flex: 1; gap: 2px; white-space: normal; }
.tool-mode-option small { color: var(--muted-foreground); font-size: var(--text-xs); }
.tool-mode-note { margin: 0; padding: var(--space-1) var(--space-2); color: var(--muted-foreground); font-size: var(--text-xs); }
</style>
