<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue"
import { RouterLink, useRoute } from "vue-router"
import { useI18n } from "vue-i18n"
import { PanelLeftClose, Plus, Search } from "lucide-vue-next"
import BrandMark from "./BrandMark.vue"
import ConnectionBadge from "./ConnectionBadge.vue"
import SidebarWorkspaceGroup from "./SidebarWorkspaceGroup.vue"
import { useSessionsStore } from "../stores/sessions"
import { useUiStore } from "../stores/ui"

defineEmits<{ navigate: [] }>()
const { t } = useI18n()
const route = useRoute()
const sessions = useSessionsStore()
const ui = useUiStore()
const search = ref("")
// Relative times re-render each minute without a reload.
const nowMs = ref(Date.now())
let clock: ReturnType<typeof setInterval> | undefined
onMounted(() => { clock = setInterval(() => { nowMs.value = Date.now() }, 60_000) })
onBeforeUnmount(() => clearInterval(clock))

const activeSessionId = computed(() => typeof route.params.sessionId === "string" ? route.params.sessionId : undefined)
const activeCwd = computed(() => activeSessionId.value === undefined ? undefined : sessions.byId.get(activeSessionId.value)?.cwd)
/** A current session seeds the new one's workspace. */
const newSessionTo = computed(() => ({ name: "chat-new" as const, ...(activeCwd.value === undefined ? {} : { query: { cwd: activeCwd.value } }) }))

const query = computed(() => search.value.trim().toLocaleLowerCase())
/** The filter matches the workspace name or path, or the session preview. */
const visible = computed(() => sessions.workspaces.flatMap((workspace) => {
  if (query.value === "") return [workspace]
  const matched = workspace.name.toLocaleLowerCase().includes(query.value) || workspace.cwd.toLocaleLowerCase().includes(query.value)
  const matching = matched ? workspace.sessions : workspace.sessions.filter((session) => session.preview?.toLocaleLowerCase().includes(query.value))
  return matching.length === 0 ? [] : [{ ...workspace, sessions: matching }]
}))
</script>

<template>
  <aside class="app-sidebar" :aria-label="t('a11y.workspaceSessionIndex')">
    <header class="brand-row">
      <BrandMark class="brand-logo" />
      <strong class="brand-copy">{{ t("brand.product") }}</strong>
      <button class="ui-icon-button sidebar-collapse" data-variant="ghost" data-size="sm" type="button" :aria-label="t('sidebar.closeMenu')" @click="ui.sidebarCollapsed = true">
        <PanelLeftClose aria-hidden="true" />
      </button>
    </header>

    <nav class="primary-actions" :aria-label="t('a11y.primaryNavigation')">
      <RouterLink class="ui-button sidebar-new" data-variant="secondary" data-size="md" :to="newSessionTo" @click="$emit('navigate')">
        <Plus aria-hidden="true" /><span class="sidebar-new-label">{{ t("sidebar.newSession") }}</span>
      </RouterLink>
    </nav>

    <div class="ui-field-wrap sidebar-search">
      <Search aria-hidden="true" />
      <input v-model="search" class="ui-field" data-size="md" type="search" :placeholder="t('sidebar.searchPlaceholder')" :aria-label="t('common.search')">
    </div>

    <div class="workspace-scroll app-scroll-viewport">
      <p class="section-label">{{ t("sidebar.workspaces") }}</p>
      <p v-if="sessions.error && !sessions.loaded" class="sidebar-empty" role="alert">{{ sessions.error }}</p>
      <p v-else-if="sessions.loaded && visible.length === 0" class="sidebar-empty">{{ t(query ? "sidebar.noMatches" : "sidebar.noWorkspaces") }}</p>
      <SidebarWorkspaceGroup
        v-for="workspace in visible"
        :key="workspace.cwd"
        :workspace="workspace"
        :expanded="query !== '' || ui.isWorkspaceExpanded(workspace.cwd)"
        :active-session-id="activeSessionId"
        :now-ms="nowMs"
        @toggle="ui.toggleWorkspace(workspace.cwd)"
        @navigate="$emit('navigate')"
      />
    </div>

    <footer class="sidebar-footer">
      <ConnectionBadge />
    </footer>
  </aside>
</template>

<style scoped>
.sidebar-new { width: 100%; justify-content: flex-start; }
.sidebar-new-label { flex: 1; text-align: left; }
</style>
