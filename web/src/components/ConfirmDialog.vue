<script setup lang="ts">
import { onBeforeUnmount, ref } from "vue"
import { useI18n } from "vue-i18n"
import { settingsMessages } from "../i18n/settings"

const { t } = useI18n({ messages: settingsMessages })
const opened = ref(false)
const message = ref("")
const label = ref("")
let settle: ((value: boolean) => void) | undefined

function open(text: string, options: { confirmLabel?: string } = {}): Promise<boolean> {
  settle?.(false)
  message.value = text
  label.value = options.confirmLabel ?? t("confirm")
  return new Promise((resolve) => {
    settle = resolve
    opened.value = true
  })
}

function finish(value: boolean): void {
  const resolve = settle
  settle = undefined
  opened.value = false
  resolve?.(value)
}

function onOpenChange(next: boolean): void {
  if (!next) finish(false)
}

onBeforeUnmount(() => settle?.(false))
defineExpose({ open })
</script>

<template>
  <UModal :open="opened" :close="false" :ui="{ content: 'sm:max-w-[560px]', footer: 'justify-end gap-2' }" @update:open="onOpenChange">
    <template #header>
      <div class="flex items-start gap-3">
        <span class="inline-flex size-9 flex-none items-center justify-center rounded-lg bg-[var(--warning-soft)] text-[var(--warning)]">
          <UIcon name="i-lucide-triangle-alert" class="size-4" aria-hidden="true" />
        </span>
        <h2 class="text-lg font-medium leading-snug text-highlighted">{{ message }}</h2>
      </div>
    </template>
    <template #footer>
      <UButton color="neutral" variant="outline" autofocus :label="t('cancel')" @click="finish(false)" />
      <UButton color="error" :label="label" @click="finish(true)" />
    </template>
  </UModal>
</template>
