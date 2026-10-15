<script setup lang="ts">
import { computed, watch } from "vue"
import { useI18n } from "vue-i18n"
import AppShell from "./AppShell.vue"
import BrandMark from "../components/BrandMark.vue"
import { useConnectionStore } from "../stores/connection"

const { t } = useI18n()
const connection = useConnectionStore()

/** Once connected, the shell stays up through reconnects and shows a banner instead. */
const showShell = computed(() => connection.everConnected && connection.phase !== "unauthenticated" && connection.phase !== "version-mismatch")
const title = computed(() => {
  if (connection.phase === "unauthenticated") return t("auth.expiredTitle")
  if (connection.phase === "version-mismatch") return t("auth.versionTitle")
  return t("auth.connectingTitle")
})

watch([showShell, title], ([shell, next]) => {
  if (!shell) document.title = `${next} — Dim`
}, { immediate: true })
</script>

<template>
  <a class="skip-link" href="#main">{{ t("common.skipToContent") }}</a>
  <AppShell v-if="showShell" />
  <main v-else-if="connection.phase === 'connecting' || connection.phase === 'reconnecting'" id="main" class="boot-page" tabindex="-1" aria-busy="true">
    <BrandMark animated class="boot-mark" />
    <h1 class="sr-only">{{ title }}</h1>
  </main>
  <main v-else id="main" class="auth-page" tabindex="-1">
    <span class="auth-signal" aria-hidden="true" />
    <p class="eyebrow">{{ t("brand.webHost") }}</p>
    <h1>{{ title }}</h1>
    <p v-if="connection.phase === 'unauthenticated'">{{ t("auth.expiredBefore") }}<code>dimcode web</code>{{ t("auth.expiredAfter") }}</p>
    <p v-else>{{ t("auth.versionDescription") }}</p>
  </main>
</template>
