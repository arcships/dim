<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, provide, ref, watch } from "vue"
import { onBeforeRouteLeave, onBeforeRouteUpdate, RouterLink, RouterView, useRoute, useRouter } from "vue-router"
import { useI18n } from "vue-i18n"
import { Activity, Archive, ArrowLeft, Box, Brain, Palette, Plug, Settings2, ShieldCheck, SlidersHorizontal, UserCircle, Wrench } from "lucide-vue-next"
import ConfirmDialog from "../../components/ConfirmDialog.vue"
import { settingsMessages } from "../../i18n/settings"
import { useConnectionStore } from "../../stores/connection"
import { useUiStore } from "../../stores/ui"
import { settingsDirtyKey } from "./context"

const { t } = useI18n({ messages: settingsMessages })
const connection = useConnectionStore()
const ui = useUiStore()
const route = useRoute()
const router = useRouter()
const dirty = ref(false)
const confirm = ref<InstanceType<typeof ConfirmDialog>>()
provide(settingsDirtyKey, dirty)
const categories = [
  { id: "account", icon: UserCircle }, { id: "appearance", icon: Palette }, { id: "agent", icon: Settings2 },
  { id: "providers", icon: Brain }, { id: "modality", icon: SlidersHorizontal }, { id: "mcp", icon: Wrench },
  { id: "plugins", icon: Plug }, { id: "skills", icon: Box }, { id: "permissions", icon: ShieldCheck },
  { id: "archived", icon: Archive }, { id: "diagnostics", icon: Activity },
]
const compact = computed(() => ui.shellMode === "compact")
const index = computed(() => route.name === "settings-index")
const category = computed(() => route.path.split("/")[2])
const parent = computed(() => route.path.split("/").length > 3 ? `/settings/${category.value}` : "/settings")
watch([compact, index], ([small, home]) => { if (!small && home) void router.replace("/settings/account") }, { immediate: true })
async function allowLeave(): Promise<boolean> {
  if (!dirty.value) return true
  const accepted = await confirm.value?.open(t("discard")) === true
  if (accepted) dirty.value = false
  return accepted
}
function beforeUnload(event: BeforeUnloadEvent): void {
  if (dirty.value) { event.preventDefault(); event.returnValue = "" }
}
onBeforeRouteLeave(allowLeave)
onBeforeRouteUpdate(allowLeave)
onMounted(() => window.addEventListener("beforeunload", beforeUnload))
onBeforeUnmount(() => window.removeEventListener("beforeunload", beforeUnload))
</script>

<template>
  <section class="settings-shell">
    <ConfirmDialog ref="confirm" />
    <aside v-if="!compact || index" class="settings-nav">
      <RouterLink class="settings-back" :to="{ name: 'root' }"><ArrowLeft :size="16" aria-hidden="true" />{{ t("back") }}</RouterLink>
      <nav :aria-label="t('settings')">
        <RouterLink v-for="item in categories" :key="item.id" :to="`/settings/${item.id}`" :class="{ 'is-active': category === item.id }" :aria-current="category === item.id ? 'page' : undefined">
          <component :is="item.icon" :size="16" aria-hidden="true" /><span>{{ t(item.id) }}</span>
        </RouterLink>
      </nav>
    </aside>
    <div v-if="!compact || !index" class="settings-body app-scroll-viewport">
      <div v-if="compact" class="settings-compact-bar">
        <RouterLink class="ui-icon-button" data-variant="ghost" :to="parent" :aria-label="t('back')"><ArrowLeft aria-hidden="true" /></RouterLink>
        <span>{{ t(category ?? 'settings') }}</span>
      </div>
      <fieldset class="settings-content" :disabled="connection.phase !== 'connected' && !['appearance', 'account', 'providers'].includes(category ?? '')">
        <RouterView v-slot="{ Component }"><component :is="Component" :key="category === 'providers' ? 'providers' : route.path" /></RouterView>
      </fieldset>
    </div>
  </section>
</template>
