<script setup lang="ts">
import { computed, ref } from "vue"
import { RouterLink } from "vue-router"
import { useI18n } from "vue-i18n"
import { ChevronRight, Folder, FolderOpen, Loader2, Plus } from "lucide-vue-next"
import type { Workspace } from "../stores/sessions"
import { formatRelativeTimeShort } from "../features/chat/timeline-time"

/** One workspace (a working directory) and its sessions, most recent first. */
const props = defineProps<{ workspace: Workspace; expanded: boolean; activeSessionId?: string; nowMs: number }>()
const emit = defineEmits<{ toggle: []; navigate: [] }>()
const { t } = useI18n()

// A group shows five sessions until "show more" walks out twenty at a time.
const FIRST = 5
const STEP = 20
const limit = ref(FIRST)
const shown = computed(() => props.workspace.sessions.slice(0, limit.value))
const hasMore = computed(() => props.workspace.sessions.length > limit.value)
const running = computed(() => !props.expanded && props.workspace.sessions.some((session) => session.running))

function toggleReveal(): void {
  limit.value = hasMore.value ? limit.value + STEP : FIRST
}
</script>

<template>
  <section class="workspace-group">
    <div class="workspace-row" @click="emit('toggle')">
      <button
        class="workspace-toggle"
        type="button"
        :aria-expanded="expanded"
        :aria-label="t(expanded ? 'sidebar.collapseWorkspace' : 'sidebar.expandWorkspace', { name: workspace.name })"
        @click.stop="emit('toggle')"
      >
        <span class="workspace-disclosure" aria-hidden="true">
          <span class="workspace-disclosure-chevron" :data-open="expanded ? 'true' : undefined"><ChevronRight :size="14" /></span>
          <span class="workspace-disclosure-icon">
            <FolderOpen v-if="expanded" :size="14" />
            <Folder v-else :size="14" />
          </span>
        </span>
        <strong :title="workspace.cwd">{{ workspace.name }}</strong>
      </button>
      <!-- A collapsed group with a run in flight shows a spinner where the actions fade in. -->
      <span v-if="running" class="workspace-running" aria-hidden="true"><Loader2 :size="14" class="is-spinning" /></span>
      <div class="workspace-row-actions" @click.stop>
        <RouterLink
          class="ui-icon-button workspace-add" data-variant="ghost" data-size="sm"
          :to="{ name: 'chat-new', query: { cwd: workspace.cwd } }"
          :aria-label="t('sidebar.newInWorkspace', { name: workspace.name })"
          v-tooltip="t('sidebar.newInWorkspace', { name: workspace.name })"
          @click="emit('navigate')"
        ><Plus aria-hidden="true" /></RouterLink>
      </div>
    </div>
    <ul v-if="expanded" class="session-list">
      <li v-for="session in shown" :key="session.sessionId">
        <RouterLink
          class="session-open"
          :to="{ name: 'chat-session', params: { sessionId: session.sessionId } }"
          :data-open="session.sessionId === activeSessionId ? 'true' : undefined"
          :aria-current="session.sessionId === activeSessionId ? 'page' : undefined"
          :title="session.preview"
          @click="emit('navigate')"
        >
          <span class="status-dot" :data-state="session.running ? 'running' : 'idle'" aria-hidden="true" />
          <strong>{{ session.preview ?? t("sidebar.untitled") }}</strong>
          <small>{{ formatRelativeTimeShort(new Date(session.updatedAt), nowMs) }}</small>
        </RouterLink>
      </li>
      <li v-if="workspace.sessions.length > FIRST">
        <button class="ui-button reveal-more" data-variant="ghost" data-size="sm" type="button" @click="toggleReveal">
          <ChevronRight :data-open="hasMore ? undefined : 'true'" aria-hidden="true" />
          {{ t(hasMore ? "sidebar.showMore" : "sidebar.showLess") }}
        </button>
      </li>
    </ul>
  </section>
</template>

<style scoped>
.workspace-row { position: relative; cursor: pointer; }
/* The disclosure is a fixed 14px slot where the folder icon cross-fades into a chevron. */
.workspace-disclosure { position: relative; display: block; width: 14px; height: 14px; flex: none; }
.workspace-disclosure-chevron,
.workspace-disclosure-icon { position: absolute; inset: 0; display: grid; place-items: center; color: var(--muted-foreground); transition: opacity var(--duration-hover) ease; }
.workspace-disclosure-chevron { opacity: 0; }
.workspace-disclosure-chevron > svg { transition: transform var(--duration-press) var(--ease-out); }
.workspace-disclosure-chevron[data-open="true"] > svg { transform: rotate(90deg); }
.workspace-row:is(:hover, :focus-within) .workspace-disclosure-chevron { opacity: 1; }
.workspace-row:is(:hover, :focus-within) .workspace-disclosure-icon { opacity: 0; }
.workspace-row-actions { display: flex; align-items: center; gap: 1px; opacity: 0; transition: opacity var(--duration-hover) ease; }
.workspace-row:is(:hover, :focus-within) .workspace-row-actions { opacity: 1; }
.workspace-running { position: absolute; top: 0; right: 0; bottom: 0; display: grid; width: var(--control-sm); place-items: center; color: var(--muted-foreground); pointer-events: none; }
.workspace-row:is(:hover, :focus-within) .workspace-running { opacity: 0; }
/* The session link reuses the row geometry of the kit's .session-open. */
.session-list .session-open { grid-template-columns: var(--control-xs) minmax(0, 1fr) auto; border-radius: var(--control-sm-radius); color: inherit; text-decoration: none; }
.session-list .session-open .status-dot { justify-self: center; }
.session-list .session-open:hover { background: var(--hover); }
.session-list .session-open[data-open="true"] { background: var(--card); box-shadow: var(--shadow-xs), 0 0 0 1px var(--border); }
@media (hover: none) {
  .workspace-row-actions { opacity: 1; }
  .workspace-running { display: none; }
}
</style>
