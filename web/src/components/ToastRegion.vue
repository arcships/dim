<script setup lang="ts">
import { useI18n } from "vue-i18n"
import { Check, Info, X } from "lucide-vue-next"
import { useToastStore, type Toast } from "../stores/toast"

const { t } = useI18n()
const toast = useToastStore()

function runAction(item: Toast): void {
  toast.dismiss(item.id)
  item.action?.run()
}
</script>

<template>
  <TransitionGroup name="ui-toast" tag="div" class="ui-toast-region" aria-live="polite">
    <div
      v-for="item in toast.toasts"
      :key="item.id"
      class="ui-toast"
      :role="item.tone === 'danger' ? 'alert' : 'status'"
      @mouseenter="toast.pause(item.id)"
      @mouseleave="toast.resume(item.id)"
      @focusin="toast.pause(item.id)"
      @focusout="toast.resume(item.id)"
    >
      <span class="ui-toast-icon" :data-tone="item.tone"><X v-if="item.tone === 'danger'" aria-hidden="true" /><Info v-else-if="item.tone === 'info'" aria-hidden="true" /><Check v-else aria-hidden="true" /></span>
      <div>
        <p class="ui-toast-title">{{ item.title }}</p>
        <p v-if="item.description" class="ui-toast-description">{{ item.description }}</p>
      </div>
      <button v-if="item.action" type="button" class="ui-button" data-variant="ghost" data-size="xs" @click="runAction(item)">{{ item.action.label }}</button>
      <button v-else type="button" class="ui-icon-button" data-variant="close" data-size="xs" :aria-label="t('common.close')" @click="toast.dismiss(item.id)"><X aria-hidden="true" /></button>
    </div>
  </TransitionGroup>
</template>
