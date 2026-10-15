<script setup lang="ts">
import { computed } from "vue"
import { useI18n } from "vue-i18n"
import { Wifi, WifiOff } from "lucide-vue-next"
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
    <Wifi v-if="connection.phase === 'connected'" :size="14" aria-hidden="true" />
    <WifiOff v-else :size="14" aria-hidden="true" />
    <span>
      <strong>{{ label }}</strong>
      <small>{{ t("connection.host") }}</small>
    </span>
  </div>
</template>
