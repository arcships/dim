<script setup lang="ts">
import { computed, ref } from "vue"
import { RouterLink } from "vue-router"
import { useI18n } from "vue-i18n"
import { Check, Shield } from "lucide-vue-next"
import ConfirmDialog from "../../components/ConfirmDialog.vue"
import type { FileAccess, PermissionChange, PermissionPolicy, ToolApproval } from "../../rpc/protocol"

/** Independent Session permission controls; process isolation stays in its existing settings. */
const props = defineProps<{ policy: PermissionPolicy | undefined }>()
const emit = defineEmits<{ select: [change: PermissionChange] }>()
const { t } = useI18n()
const details = ref<HTMLDetailsElement>()
const confirm = ref<InstanceType<typeof ConfirmDialog>>()

const FILES: FileAccess[] = ["read-only", "workspace-read-write", "unrestricted"]
const TOOLS: ToolApproval[] = ["ask", "bypass"]
const fileAccess = computed(() => {
  const policy = props.policy
  if (policy === undefined) return undefined
  return policy.fileAccess ?? ({ "read-only": "read-only", "workspace-write": "workspace-read-write", "full-access": "unrestricted" } as const)[policy.preset]
})
const toolApproval = computed(() => props.policy?.toolApproval ?? (props.policy?.preset === "full-access" ? "bypass" : undefined))
const toolLabel = computed(() => toolApproval.value === undefined ? t("composer.permission.legacy") : t(`composer.permission.tool.${toolApproval.value}`))

async function choose(change: PermissionChange): Promise<void> {
  if (props.policy === undefined) return
  const widensFiles = change.field === "fileAccess" && change.value === "unrestricted" && fileAccess.value !== "unrestricted"
  const bypassesTools = change.field === "toolApproval" && change.value === "bypass" && toolApproval.value !== "bypass"
  if (widensFiles || bypassesTools) {
    const message = widensFiles ? "composer.permission.unrestrictedConfirm" : "composer.permission.bypassConfirm"
    if (!await confirm.value?.open(t(message))) return
  }
  emit("select", change)
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
      <span class="permission-trigger-label">{{ policy && fileAccess ? `${t(`composer.permission.file.${fileAccess}`)} · ${toolLabel}` : t("composer.permission.label") }}</span>
    </summary>
    <div class="composer-popover-content ui-popover permission-content" data-width="lg">
      <fieldset class="permission-group" :disabled="!policy">
        <legend>{{ t("composer.permission.fileAccess") }}</legend>
        <button v-for="value in FILES" :key="value" type="button" class="ui-menu-item permission-option" :aria-pressed="value === fileAccess" @click="choose({ field: 'fileAccess', value })">
          <span class="permission-option-text"><span>{{ t(`composer.permission.file.${value}`) }}</span><small>{{ t(`composer.permission.fileDescription.${value}`) }}</small></span>
          <Check v-if="value === fileAccess" :size="14" aria-hidden="true" />
        </button>
      </fieldset>
      <fieldset class="permission-group" :disabled="!policy">
        <legend>{{ t("composer.permission.toolApproval") }}</legend>
        <p v-if="policy && toolApproval === undefined" class="permission-note">{{ t(`composer.permission.legacy.${policy.preset}`) }}</p>
        <button v-for="value in TOOLS" :key="value" type="button" class="ui-menu-item permission-option" :aria-pressed="value === toolApproval" @click="choose({ field: 'toolApproval', value })">
          <span class="permission-option-text"><span>{{ t(`composer.permission.tool.${value}`) }}</span><small>{{ t(`composer.permission.toolDescription.${value}`) }}</small></span>
          <Check v-if="value === toolApproval" :size="14" aria-hidden="true" />
        </button>
      </fieldset>
      <p v-if="policy?.allowedTools.length" class="permission-note">{{ t("composer.permission.allowedTools") }}: {{ policy.allowedTools.join(', ') }}</p>
      <p v-if="policy?.disallowedTools.length" class="permission-note">{{ t("composer.permission.disallowedTools") }}: {{ policy.disallowedTools.join(', ') }}</p>
      <p class="permission-note">{{ t("composer.permission.independent") }}</p>
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
