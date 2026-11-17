<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue"
import { RouterView, useRoute } from "vue-router"
import { useI18n } from "vue-i18n"
import AppSidebar from "../components/AppSidebar.vue"
import ToastRegion from "../components/ToastRegion.vue"
import EndLineDialog from "../features/lines/EndLineDialog.vue"
import ExtensionPanel from "../features/extensions/ExtensionPanel.vue"
import LinesPanel from "../features/lines/LinesPanel.vue"
import NewLineDialog from "../features/lines/NewLineDialog.vue"
import { settingsMessages } from "../i18n/settings"
import { useConnectionStore } from "../stores/connection"
import { useSessionsStore, workspaceName } from "../stores/sessions"
import { useUiStore } from "../stores/ui"

const route = useRoute()
const { t } = useI18n()
const { t: settingsText } = useI18n({ messages: settingsMessages })
const connection = useConnectionStore()
const sessions = useSessionsStore()
const ui = useUiStore()
const root = ref<HTMLElement>()
let observer: ResizeObserver | undefined

const settingsRoute = computed(() => route.path === "/settings" || route.path.startsWith("/settings/"))
const chatRoute = computed(() => route.name === "chat-new" || route.name === "chat-session")
const sessionId = computed(() => typeof route.params.sessionId === "string" ? route.params.sessionId : undefined)
const detailOpen = computed(() => ui.detailOpen && route.name === "chat-session" && sessionId.value !== undefined)
const pageTitle = computed(() => {
  if (settingsRoute.value) return settingsText("settings")
  if (route.name === "chat-session" && typeof route.params.sessionId === "string") {
    const session = sessions.byId.get(route.params.sessionId)
    return session?.title ?? session?.preview ?? (session === undefined ? t("route.session") : workspaceName(session.cwd))
  }
  if (route.name === "not-found") return t("route.notFound")
  return t("route.newSession")
})

watch(pageTitle, (title) => { document.title = `${title} — Dim` }, { immediate: true })

function onKey(event: KeyboardEvent): void {
  // ⌘B / Ctrl+B toggles the sidebar: the drawer in compact mode, the inline rail otherwise.
  const mod = navigator.userAgent.includes("Mac") ? event.metaKey : event.ctrlKey
  if (mod && !event.altKey && !event.shiftKey && event.key.toLowerCase() === "b") {
    event.preventDefault()
    if (ui.shellMode === "compact") ui.drawerOpen = !ui.drawerOpen
    else ui.sidebarCollapsed = !ui.sidebarCollapsed
  }
}

/** The directory has no change notification; coming back to the tab refreshes it. */
function onFocus(): void {
  if (connection.phase === "connected") void sessions.refresh()
}

onMounted(() => {
  if (root.value !== undefined) {
    observer = new ResizeObserver(([entry]) => {
      if (entry !== undefined) ui.setShellWidth(entry.contentRect.width)
    })
    observer.observe(root.value)
  }
  window.addEventListener("keydown", onKey)
  window.addEventListener("focus", onFocus)
})

onBeforeUnmount(() => {
  observer?.disconnect()
  window.removeEventListener("keydown", onKey)
  window.removeEventListener("focus", onFocus)
})

function startResize(event: PointerEvent): void {
  if (ui.shellMode === "compact") return
  const startX = event.clientX
  const startWidth = ui.detailWidth
  const move = (next: PointerEvent): void => {
    ui.setDetailWidth(startWidth + startX - next.clientX)
  }
  const stop = (): void => {
    window.removeEventListener("pointermove", move)
    window.removeEventListener("pointerup", stop)
  }
  window.addEventListener("pointermove", move)
  window.addEventListener("pointerup", stop, { once: true })
}

function resizeBy(delta: number): void {
  ui.setDetailWidth(ui.detailWidth + delta)
}
</script>

<template>
  <div
    ref="root"
    class="app-shell"
    :data-mode="ui.shellMode"
    :data-sidebar-collapsed="ui.sidebarCollapsed"
    :data-chat-route="chatRoute"
    :data-settings-route="settingsRoute"
    style="--sidebar-width: 320px"
  >
    <ToastRegion />

    <header v-if="ui.shellMode === 'compact' && !settingsRoute" class="compact-header">
      <UButton icon="i-lucide-menu" variant="ghost" color="neutral" size="md" square :aria-label="t('sidebar.openMenu')" @click="ui.drawerOpen = true" />
      <span class="compact-title">{{ pageTitle }}</span>
    </header>

    <UDrawer
      v-if="ui.shellMode === 'compact'"
      v-model:open="ui.drawerOpen"
      direction="left"
      :handle="false"
      :title="t('sidebar.workspaces')"
      :ui="{ content: 'p-0 w-[88vw] max-w-[340px] rounded-none', body: 'p-0 flex-1 min-h-0' }"
    >
      <template #content>
        <div class="relative flex h-full w-full flex-col">
          <UButton
            autofocus
            class="drawer-close"
            icon="i-lucide-x"
            variant="ghost"
            color="neutral"
            size="md"
            square
            :aria-label="t('sidebar.closeMenu')"
            @click="ui.drawerOpen = false"
          />
          <AppSidebar @navigate="ui.drawerOpen = false" />
        </div>
      </template>
    </UDrawer>

    <AppSidebar v-if="ui.shellMode !== 'compact' && !ui.sidebarCollapsed && !settingsRoute" class="inline-sidebar" />

    <div
      class="content-region"
      :data-has-detail="detailOpen"
      :data-detail-expanded="ui.detailExpanded"
      :style="{ '--detail-width': `${ui.detailWidth}px` }"
    >
      <main id="main" class="main-region" :aria-label="t('a11y.mainContent')" @pointerdown="ui.detailFocused = false" @focusin="ui.detailFocused = false">
        <UTooltip v-if="ui.sidebarCollapsed && ui.shellMode !== 'compact' && !settingsRoute" :text="t('sidebar.openMenu')">
          <UButton
            class="sidebar-reopen"
            icon="i-lucide-panel-left-open"
            variant="ghost"
            color="neutral"
            size="md"
            square
            :aria-label="t('sidebar.openMenu')"
            @click="ui.sidebarCollapsed = false"
          />
        </UTooltip>
        <header v-if="!settingsRoute" class="main-header">
          <div><p>Dim {{ connection.hello?.serverVersion }}</p><strong>{{ pageTitle }}</strong></div>
        </header>
        <UAlert
          v-if="connection.phase === 'reconnecting'"
          class="reconnect-banner rounded-none"
          color="warning"
          variant="subtle"
          icon="i-lucide-wifi-off"
          :description="t('page.reconnecting')"
          role="status"
        />
        <RouterView :key="settingsRoute ? 'settings' : route.fullPath" />
      </main>

      <div
        v-if="detailOpen"
        class="detail-resizer"
        role="separator"
        aria-orientation="vertical"
        :aria-label="t('lines.resize')"
        :aria-valuenow="ui.detailWidth"
        :aria-valuemin="320"
        :aria-valuemax="720"
        tabindex="0"
        @pointerdown.prevent="startResize"
        @keydown.left.prevent="resizeBy(-16)"
        @keydown.right.prevent="resizeBy(16)"
      />
      <aside v-if="detailOpen && sessionId" class="detail-region" data-active="true" :aria-label="ui.extension ? undefined : t('lines.panelAria')" @pointerdown="ui.detailFocused = true" @focusin="ui.detailFocused = true">
        <ExtensionPanel v-if="ui.extension" :session-id="sessionId" />
        <LinesPanel v-else :session-id="sessionId" />
      </aside>
    </div>

    <NewLineDialog v-if="ui.newLine" />
    <EndLineDialog v-if="ui.endingLine" />
  </div>
</template>
