<script setup lang="ts">
import { computed, ref } from "vue"
import { useI18n } from "vue-i18n"
import { useChatStore } from "../../stores/chat"
import { useUiStore } from "../../stores/ui"

const { t } = useI18n()
const chat = useChatStore()
const ui = useUiStore()
const error = ref("")
const ending = ref(false)
const request = computed(() => ui.endingLine)
const open = computed({
  get: () => request.value !== undefined,
  set: (value: boolean) => {
    if (!value) ui.closeEndLine()
  },
})

function close(): void {
  ui.closeEndLine()
}

async function end(): Promise<void> {
  const current = request.value
  if (current === undefined || ending.value) return
  ending.value = true
  error.value = ""
  try {
    await chat.endLine(current.sessionId, current.branchId)
    close()
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    ending.value = false
  }
}
</script>

<template>
  <UModal v-model:open="open" :title="request ? t('lines.endTitle', { name: request.purpose }) : ''" :ui="{ footer: 'justify-end' }">
    <template #header>
      <div v-if="request" class="end-line-header">
        <span class="end-line-icon" aria-hidden="true"><UIcon name="i-lucide-git-branch" class="size-4" /></span>
        <div class="min-w-0 flex-1">
          <h2 class="end-line-title">{{ t("lines.endTitle", { name: request.purpose }) }}</h2>
          <p class="end-line-description">{{ t("lines.endDescription") }}</p>
        </div>
      </div>
    </template>

    <template v-if="error" #body>
      <UAlert color="error" variant="subtle" :description="error" />
    </template>

    <template #footer>
      <UButton color="neutral" variant="ghost" size="md" :disabled="ending" @click="close">{{ t("common.cancel") }}</UButton>
      <UButton color="error" variant="solid" size="md" :loading="ending" :disabled="ending" @click="end">{{ t("lines.end") }}</UButton>
    </template>
  </UModal>
</template>

<style scoped>
.end-line-header { display: flex; align-items: flex-start; gap: var(--space-3); }
.end-line-icon { display: grid; width: 32px; height: 32px; flex: none; place-items: center; border-radius: var(--radius-lg); background: color-mix(in oklab, var(--ui-error) 14%, transparent); color: var(--ui-error); }
.end-line-title { margin: 0; font-size: var(--text-base); font-weight: var(--weight-semibold); }
.end-line-description { margin: var(--space-1) 0 0; color: var(--muted-foreground); font-size: var(--text-sm); }
</style>
