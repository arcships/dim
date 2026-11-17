<script setup lang="ts">
import { computed } from "vue"
import { useI18n } from "vue-i18n"
import { useConnectionStore } from "../stores/connection"

const connection = useConnectionStore()
const { t } = useI18n()
const label = computed(() => ({
  connecting: t("connection.connecting"),
  connected: t("connection.live"),
  reconnecting: t("connection.reconnecting"),
  unauthenticated: t("connection.accessRequired"),
  "version-mismatch": t("connection.versionMismatch"),
})[connection.phase])
</script>

<template>
  <div class="connection-badge" aria-live="polite" :data-state="connection.phase">
    <UIcon :name="connection.phase === 'connected' ? 'i-lucide-wifi' : 'i-lucide-wifi-off'" class="size-4 flex-none" aria-hidden="true" />
    <span>
      <strong>{{ label }}</strong>
      <small>{{ t("connection.host") }}</small>
    </span>
  </div>
</template>
