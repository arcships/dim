<script setup lang="ts">
import { watch } from "vue"
import { useRouter } from "vue-router"
import { useI18n } from "vue-i18n"
import { useSessionsStore } from "../stores/sessions"
import { useUiStore } from "../stores/ui"

const { t } = useI18n()
const router = useRouter()
const sessions = useSessionsStore()
const ui = useUiStore()

// `/` is only a decision: the last session when it still exists, otherwise a new one.
watch(() => sessions.loaded || sessions.error !== "", (settled) => {
  if (!settled) return
  const last = ui.lastSessionId
  void router.replace(last !== undefined && sessions.byId.has(last) ? { name: "chat-session", params: { sessionId: last } } : { name: "chat-new" })
}, { immediate: true })
</script>

<template>
  <section class="chat-loading" aria-busy="true"><p>{{ t("common.loading") }}</p></section>
</template>
