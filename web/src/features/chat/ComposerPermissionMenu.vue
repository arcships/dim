<script setup lang="ts">
import { ref } from "vue"
import { RouterLink } from "vue-router"
import { useI18n } from "vue-i18n"
import { Check, Shield } from "lucide-vue-next"
import ConfirmDialog from "../../components/ConfirmDialog.vue"
import type { PermissionPolicy, PermissionPreset } from "../../rpc/protocol"

/** Session permission preset; process isolation stays in Settings. */
const props = defineProps<{ policy: PermissionPolicy | undefined }>()
const emit = defineEmits<{ select: [preset: PermissionPreset] }>()
const { t } = useI18n()
const details = ref<HTMLDetailsElement>()
const confirm = ref<InstanceType<typeof ConfirmDialog>>()

const PRESETS: PermissionPreset[] = ["read-only", "workspace-write", "full-access"]

async function choose(preset: PermissionPreset): Promise<void> {
  if (props.policy === undefined || preset === props.policy.preset) return
  if (preset === "full-access" && !await confirm.value?.open(t("composer.permission.fullAccessConfirm"))) return
  emit("select", preset)
}

function open(): void {
  if (details.value) details.value.open = true
}

defineExpose({ open })
</script>

<template>
  <details ref="details" class="composer-popover permission-menu">
    <summary class="ui-button permission-trigger" data-variant="ghost" data-size="sm" :aria-label="t('composer.permission.label')">
      <Shield :size="14" aria-hidden="true" />
      <span class="permission-trigger-label">{{ policy ? t(`composer.permission.preset.${policy.preset}`) : t("composer.permission.label") }}</span>
    </summary>
    <div class="composer-popover-content ui-popover permission-content" data-width="lg">
      <fieldset class="permission-group" :disabled="!policy">
        <legend>{{ t("composer.permission.label") }}</legend>
        <button v-for="preset in PRESETS" :key="preset" type="button" class="ui-menu-item permission-option" :aria-pressed="preset === policy?.preset" @click="choose(preset)">
          <span class="permission-option-text"><span>{{ t(`composer.permission.preset.${preset}`) }}</span><small>{{ t(`composer.permission.description.${preset}`) }}</small></span>
          <Check v-if="preset === policy?.preset" :size="14" aria-hidden="true" />
        </button>
      </fieldset>
      <p v-if="policy?.allowedTools.length" class="permission-note">{{ t("composer.permission.allowedTools") }}: {{ policy.allowedTools.join(', ') }}</p>
      <p v-if="policy?.disallowedTools.length" class="permission-note">{{ t("composer.permission.disallowedTools") }}: {{ policy.disallowedTools.join(', ') }}</p>
      <p class="permission-note">{{ t("composer.permission.isolation") }}</p>
      <RouterLink class="ui-menu-item" :to="{ name: 'settings-permissions' }">{{ t("composer.permission.processIsolation") }}</RouterLink>
    </div>
    <ConfirmDialog ref="confirm" />
  </details>
</template>

<style scoped>
.permission-trigger { gap: var(--space-1); }
.permission-trigger-label { max-width: 260px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.permission-content { display: grid; gap: var(--space-2); max-height: 70dvh; overflow-y: auto; }
.permission-group { min-width: 0; margin: 0; padding: 0; border: 0; }
.permission-group legend { padding: var(--space-1) var(--space-2); font-size: var(--text-xs); font-weight: 600; }
.permission-option { display: flex; width: 100%; align-items: flex-start; gap: var(--space-2); text-align: left; }
.permission-option-text { display: grid; min-width: 0; flex: 1; gap: 2px; }
.permission-option small { color: var(--muted-foreground); font-size: var(--text-xs); }
.permission-note { margin: 0; padding: var(--space-1) var(--space-2); color: var(--muted-foreground); font-size: var(--text-xs); overflow-wrap: anywhere; }
</style>
