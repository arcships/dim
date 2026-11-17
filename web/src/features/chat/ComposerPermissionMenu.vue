<script setup lang="ts">
import { ref } from "vue"
import { useI18n } from "vue-i18n"
import ConfirmDialog from "../../components/ConfirmDialog.vue"
import type { PermissionPolicy, PermissionPreset } from "../../rpc/protocol"

/** Session permission preset; process isolation stays in Settings. */
const props = defineProps<{ policy: PermissionPolicy | undefined }>()
const emit = defineEmits<{ select: [preset: PermissionPreset] }>()
const { t } = useI18n()
const popoverOpen = ref(false)
const confirm = ref<InstanceType<typeof ConfirmDialog>>()

const PRESETS: PermissionPreset[] = ["read-only", "workspace-write", "full-access"]

async function choose(preset: PermissionPreset): Promise<void> {
  if (props.policy === undefined || preset === props.policy.preset) return
  if (preset === "full-access" && !await confirm.value?.open(t("composer.permission.fullAccessConfirm"))) return
  emit("select", preset)
}

function open(): void {
  popoverOpen.value = true
}

defineExpose({ open })
</script>

<template>
  <UPopover v-model:open="popoverOpen" :content="{ side: 'top', align: 'start' }">
    <UButton color="neutral" variant="ghost" size="sm" icon="i-lucide-shield" class="permission-trigger" :aria-label="t('composer.permission.label')">
      <span class="permission-trigger-label">{{ policy ? t(`composer.permission.preset.${policy.preset}`) : t("composer.permission.label") }}</span>
    </UButton>
    <template #content>
      <div class="permission-content">
        <fieldset class="permission-group" :disabled="!policy">
          <legend>{{ t("composer.permission.label") }}</legend>
          <UButton
            v-for="preset in PRESETS"
            :key="preset"
            color="neutral"
            :variant="preset === policy?.preset ? 'soft' : 'ghost'"
            size="sm"
            block
            class="permission-option"
            :aria-pressed="preset === policy?.preset"
            @click="choose(preset)"
          >
            <span class="permission-option-text"><span>{{ t(`composer.permission.preset.${preset}`) }}</span><small>{{ t(`composer.permission.description.${preset}`) }}</small></span>
            <template #trailing>
              <UIcon v-if="preset === policy?.preset" name="i-lucide-check" class="size-3.5 shrink-0 text-primary" aria-hidden="true" />
            </template>
          </UButton>
        </fieldset>
        <p v-if="policy?.allowedTools.length" class="permission-note">{{ t("composer.permission.allowedTools") }}: {{ policy.allowedTools.join(', ') }}</p>
        <p v-if="policy?.disallowedTools.length" class="permission-note">{{ t("composer.permission.disallowedTools") }}: {{ policy.disallowedTools.join(', ') }}</p>
        <p class="permission-note">{{ t("composer.permission.isolation") }}</p>
        <UButton color="neutral" variant="ghost" size="sm" block class="justify-start" :to="{ name: 'settings-permissions' }">{{ t("composer.permission.processIsolation") }}</UButton>
      </div>
    </template>
  </UPopover>
  <ConfirmDialog ref="confirm" />
</template>

<style scoped>
.permission-trigger-label { max-width: 260px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.permission-content { display: grid; width: min(360px, calc(100vw - 24px)); gap: var(--space-2); max-height: 70dvh; overflow-y: auto; padding: var(--space-2); }
.permission-group { display: grid; gap: 2px; min-width: 0; margin: 0; padding: 0; border: 0; }
.permission-group legend { padding: var(--space-1) var(--space-2); font-size: var(--text-xs); font-weight: 600; }
.permission-option { justify-content: flex-start; text-align: left; }
.permission-option-text { display: grid; min-width: 0; flex: 1; gap: 2px; white-space: normal; }
.permission-option small { color: var(--muted-foreground); font-size: var(--text-xs); }
.permission-note { margin: 0; padding: var(--space-1) var(--space-2); color: var(--muted-foreground); font-size: var(--text-xs); overflow-wrap: anywhere; }
</style>
