<script setup lang="ts">
import { RouterLink } from "vue-router"
import { useI18n } from "vue-i18n"
import { CircleSlash2 } from "lucide-vue-next"

const { t } = useI18n()
</script>

<template>
  <section class="route-page app-scroll-viewport">
    <CircleSlash2 :size="20" class="state-icon" aria-hidden="true" />
    <h1 data-route-heading tabindex="-1">{{ t("route.notFound") }}</h1>
    <p class="page-description">{{ t("page.notFoundDescription") }}</p>
    <RouterLink to="/chat/new">{{ t("sidebar.newSession") }}</RouterLink>
  </section>
</template>
