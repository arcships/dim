<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue"
import { useRoute, useRouter } from "vue-router"
import { useI18n } from "vue-i18n"
import { settingsMessages } from "../i18n/settings"
import BrandMark from "./BrandMark.vue"
import ConnectionBadge from "./ConnectionBadge.vue"
import SidebarWorkspaceGroup from "./SidebarWorkspaceGroup.vue"
import { sortKey } from "./sidebar-sort"
import { useArchiveStore } from "../stores/archive"
import { groupWorkspaces, useSessionsStore } from "../stores/sessions"
import { useToastStore } from "../stores/toast"
import { useUiStore } from "../stores/ui"

const emit = defineEmits<{ navigate: [] }>()
const { t } = useI18n()
const { t: settingsText } = useI18n({ messages: settingsMessages })
const route = useRoute()
const router = useRouter()
const sessions = useSessionsStore()
const archive = useArchiveStore()
const toast = useToastStore()
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
/** Hidden rows are dropped before grouping and search; workspaces and their sessions order by the chosen sort. */
const visible = computed(() => groupWorkspaces(sessions.summaries.filter((session) => !archive.has(session.sessionId))).flatMap((workspace) => {
  const matched = query.value === "" || workspace.name.toLocaleLowerCase().includes(query.value) || workspace.cwd.toLocaleLowerCase().includes(query.value)
  const matching = query.value === "" || matched ? workspace.sessions : workspace.sessions.filter((session) => [session.title, session.preview].some((text) => text?.toLocaleLowerCase().includes(query.value)))
  if (matching.length === 0) return []
  return [{ ...workspace, sessions: [...matching].sort((left, right) => sortKey(right, ui.sidebarSortBy) - sortKey(left, ui.sidebarSortBy)) }]
}).sort((left, right) => sortKey(right.sessions[0]!, ui.sidebarSortBy) - sortKey(left.sessions[0]!, ui.sidebarSortBy)))

function archiveSession(sessionId: string): void {
  if (!archive.archive(sessionId)) {
    toast.push({ tone: "danger", title: t("sidebar.archiveFailed") })
    return
  }
  if (sessionId === activeSessionId.value) {
    void router.push(newSessionTo.value)
    emit("navigate")
  }
}

const sortItems = computed(() => (["recentActive", "recentCreated"] as const).map((value) => ({
  label: t(`sidebar.${value}`),
  icon: "i-lucide-check",
  ui: { itemLeadingIcon: ui.sidebarSortBy === value ? undefined : "opacity-0" },
  onSelect: () => { ui.sidebarSortBy = value },
})))

</script>

<template>
  <aside class="app-sidebar" :aria-label="t('a11y.workspaceSessionIndex')">
    <header class="brand-row">
      <BrandMark class="brand-logo" />
      <strong class="brand-copy">{{ t("brand.product") }}</strong>
      <UButton
        class="sidebar-collapse"
        icon="i-lucide-panel-left-close"
        variant="ghost"
        color="neutral"
        size="sm"
        square
        :aria-label="t('sidebar.closeMenu')"
        @click="ui.sidebarCollapsed = true"
      />
    </header>

    <nav class="primary-actions" :aria-label="t('a11y.primaryNavigation')">
      <UButton
        class="justify-start"
        block
        variant="outline"
        color="neutral"
        size="md"
        icon="i-lucide-plus"
        :to="newSessionTo"
        @click="$emit('navigate')"
      >
        <span class="flex-1 text-left">{{ t("sidebar.newSession") }}</span>
      </UButton>
    </nav>

    <div class="sidebar-search-row">
      <UInput
        v-model="search"
        class="sidebar-search"
        size="md"
        type="search"
        icon="i-lucide-search"
        variant="soft"
        :placeholder="t('sidebar.searchPlaceholder')"
        :aria-label="t('common.search')"
      />
      <UDropdownMenu :items="[sortItems]" :content="{ align: 'end' }">
        <UTooltip :text="t('sidebar.sort')">
          <UButton
            color="neutral"
            variant="ghost"
            size="md"
            square
            icon="i-lucide-arrow-up-down"
            :aria-label="t('sidebar.sort')"
          />
        </UTooltip>
      </UDropdownMenu>
    </div>

    <div class="workspace-scroll app-scroll-viewport">
      <p class="section-label">{{ t("sidebar.workspaces") }}</p>
      <p v-if="sessions.error && !sessions.loaded" class="sidebar-empty" role="alert">{{ sessions.error }}</p>
      <p v-else-if="sessions.loaded && visible.length === 0" class="sidebar-empty">{{ t(query ? "sidebar.noMatches" : "sidebar.noWorkspaces") }}</p>
      <SidebarWorkspaceGroup
        v-for="workspace in visible"
        :key="`${ui.sidebarSortBy}:${workspace.cwd}`"
        :workspace="workspace"
        :expanded="query !== '' || ui.isWorkspaceExpanded(workspace.cwd)"
        :active-session-id="activeSessionId"
        :now-ms="nowMs"
        @toggle="ui.toggleWorkspace(workspace.cwd)"
        @archive="archiveSession"
        @navigate="$emit('navigate')"
      />
    </div>

    <footer class="sidebar-footer">
      <UButton
        class="justify-start"
        block
        variant="ghost"
        color="neutral"
        size="md"
        icon="i-lucide-settings"
        to="/settings"
        @click="$emit('navigate')"
      >
        {{ settingsText("settings") }}
      </UButton>
      <ConnectionBadge />
    </footer>
  </aside>
</template>
