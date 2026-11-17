<script setup lang="ts">
import { useI18n } from "vue-i18n"

const { t } = useI18n()
</script>

<template>
  <section class="route-page app-scroll-viewport">
    <UIcon name="i-lucide-circle-slash-2" class="state-icon size-5" aria-hidden="true" />
    <h1 data-route-heading tabindex="-1">{{ t("route.notFound") }}</h1>
    <p class="page-description">{{ t("page.notFoundDescription") }}</p>
    <UButton variant="link" color="primary" to="/chat/new" :label="t('sidebar.newSession')" />
  </section>
</template>
