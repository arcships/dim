<script setup lang="ts">
import { computed, ref, watch } from "vue"
import { useI18n } from "vue-i18n"
import { MAIN_BRANCH } from "../../rpc/protocol"
import { useChatStore } from "../../stores/chat"
import { useUiStore } from "../../stores/ui"
import { lineName, shortId } from "./line-state"

const { t } = useI18n()
const chat = useChatStore()
const ui = useUiStore()
const prompt = ref("")
const error = ref("")
const creating = ref(false)
const request = computed(() => ui.newLine)
const open = computed({
  get: () => request.value !== undefined,
  set: (value: boolean) => {
    if (!value) ui.closeNewLine()
  },
})

const parentName = computed(() => {
  const current = request.value
  if (current === undefined) return ""
  if (current.parentBranchId === MAIN_BRANCH) return t("lines.main")
  const parent = chat.views[current.sessionId]?.lines.find((line) => line.id === current.parentBranchId)
  return parent === undefined ? shortId(current.parentBranchId) : lineName(t, parent, current.parentBranchId)
})

const cutLabel = computed(() => {
  const current = request.value
  if (current === undefined) return ""
  const before = current.before ?? current.text.split("\n")[0]?.trim() ?? ""
  return before === "" ? t("lines.atEnd") : t("lines.beforeMessage", { message: before })
})

function reset(): void {
  prompt.value = request.value?.text ?? ""
  error.value = ""
  creating.value = false
}

watch(request, reset, { immediate: true })

function close(): void {
  ui.closeNewLine()
}

async function create(): Promise<void> {
  const current = request.value
  if (current === undefined || creating.value) return
  const text = prompt.value.trim()
  if (text === "") {
    error.value = t("lines.promptRequired")
    return
  }
  creating.value = true
  error.value = ""
  try {
    const branchId = await chat.createLine(current.sessionId, current.parentBranchId, current.at, text)
    ui.openLine(current.sessionId, branchId)
    close()
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    creating.value = false
  }
}
</script>

<template>
  <UModal v-model:open="open" :title="t('lines.newLineTitle')" :ui="{ footer: 'justify-end' }">
    <template #header>
      <div class="new-line-header">
        <span class="new-line-icon" aria-hidden="true"><UIcon name="i-lucide-git-branch" class="size-4" /></span>
        <div class="min-w-0 flex-1">
          <h2 class="new-line-title">{{ t("lines.newLineTitle") }}</h2>
          <p class="new-line-description">
            {{ t("lines.newLineFrom", { name: parentName }) }}<br>
            <span class="new-line-cut">{{ cutLabel }}</span>
          </p>
        </div>
      </div>
    </template>

    <template #body>
      <form v-if="request" id="new-line-form" @submit.prevent="create">
        <UFormField :label="t('lines.promptLabel')">
          <UTextarea
            v-model="prompt"
            :rows="4"
            class="w-full"
            :placeholder="t('lines.promptPlaceholder')"
            autofocus
          />
        </UFormField>
        <UAlert v-if="error" color="error" variant="subtle" :description="error" class="mt-3" />
      </form>
    </template>

    <template #footer>
      <UButton color="neutral" variant="ghost" size="md" :disabled="creating" @click="close">{{ t("common.cancel") }}</UButton>
      <UButton type="submit" form="new-line-form" color="primary" variant="solid" size="md" :loading="creating" :disabled="creating || prompt.trim() === ''">
        {{ creating ? t("common.loading") : t("lines.create") }}
      </UButton>
    </template>
  </UModal>
</template>

<style scoped>
.new-line-header { display: flex; align-items: flex-start; gap: var(--space-3); }
.new-line-icon { display: grid; width: 32px; height: 32px; flex: none; place-items: center; border-radius: var(--radius-lg); background: color-mix(in oklab, var(--ui-primary) 14%, transparent); color: var(--ui-primary); }
.new-line-title { margin: 0; font-size: var(--text-base); font-weight: var(--weight-semibold); }
.new-line-description { margin: var(--space-1) 0 0; color: var(--muted-foreground); font-size: var(--text-sm); }
.new-line-cut { color: var(--muted-foreground); font-family: var(--font-mono); font-size: var(--text-xs); }
</style>
