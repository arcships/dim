<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue"
import { RouterView, useRoute } from "vue-router"
import { useI18n } from "vue-i18n"
import { Menu, PanelLeftOpen, X } from "lucide-vue-next"
import AppSidebar from "../components/AppSidebar.vue"
import ToastRegion from "../components/ToastRegion.vue"
import { useConnectionStore } from "../stores/connection"
import { useSessionsStore, workspaceName } from "../stores/sessions"
import { useUiStore } from "../stores/ui"

const route = useRoute()
const { t } = useI18n()
const connection = useConnectionStore()
const sessions = useSessionsStore()
const ui = useUiStore()
const root = ref<HTMLElement>()
const drawer = ref<HTMLDialogElement>()
let observer: ResizeObserver | undefined

const chatRoute = computed(() => route.name === "chat-new" || route.name === "chat-session")
const pageTitle = computed(() => {
  if (route.name === "chat-session" && typeof route.params.sessionId === "string") {
    const session = sessions.byId.get(route.params.sessionId)
    return session?.preview ?? (session === undefined ? t("route.session") : workspaceName(session.cwd))
  }
  if (route.name === "not-found") return t("route.notFound")
  return t("route.newSession")
})

watch(pageTitle, (title) => { document.title = `${title} — Dim` }, { immediate: true })
watch(() => ui.drawerOpen, async (open) => {
  await nextTick()
  const element = drawer.value
  if (element === undefined) return
  if (open && !element.open) element.showModal()
  else if (!open && element.open) element.close()
})

/**
 * Menus and popovers are native `<details>`, which only close from their own
 * summary; a press anywhere else or Escape dismisses them.
 */
const POPOVERS = "details.session-menu[open], details.composer-popover[open]"

function closeMenusOutside(event: PointerEvent): void {
  const target = event.target instanceof Node ? event.target : undefined
  for (const menu of document.querySelectorAll<HTMLDetailsElement>(POPOVERS)) {
    if (target === undefined || !menu.contains(target)) menu.open = false
  }
}

function onKey(event: KeyboardEvent): void {
  if (event.key === "Escape") {
    for (const menu of document.querySelectorAll<HTMLDetailsElement>(POPOVERS)) menu.open = false
    return
  }
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
  document.addEventListener("pointerdown", closeMenusOutside, true)
})

onBeforeUnmount(() => {
  observer?.disconnect()
  window.removeEventListener("keydown", onKey)
  window.removeEventListener("focus", onFocus)
  document.removeEventListener("pointerdown", closeMenusOutside, true)
})

function closeDrawerFromBackdrop(event: MouseEvent): void {
  if (event.target === drawer.value) ui.drawerOpen = false
}
</script>

<template>
  <div
    ref="root"
    class="app-shell"
    :data-mode="ui.shellMode"
    :data-sidebar-collapsed="ui.sidebarCollapsed"
    :data-chat-route="chatRoute"
    style="--sidebar-width: 280px"
  >
    <ToastRegion />

    <header v-if="ui.shellMode === 'compact'" class="compact-header">
      <button class="ui-icon-button" data-variant="ghost" data-size="md" type="button" :aria-label="t('sidebar.openMenu')" @click="ui.drawerOpen = true">
        <Menu aria-hidden="true" />
      </button>
      <span class="compact-title">{{ pageTitle }}</span>
    </header>

    <dialog v-if="ui.shellMode === 'compact'" ref="drawer" class="drawer-content" @click="closeDrawerFromBackdrop" @close="ui.drawerOpen = false">
      <h2 class="sr-only">{{ t("sidebar.workspaces") }}</h2>
      <button autofocus class="ui-icon-button drawer-close" data-variant="ghost" data-size="md" type="button" :aria-label="t('sidebar.closeMenu')" @click="ui.drawerOpen = false">
        <X aria-hidden="true" />
      </button>
      <AppSidebar @navigate="ui.drawerOpen = false" />
    </dialog>

    <AppSidebar v-if="ui.shellMode !== 'compact' && !ui.sidebarCollapsed" class="inline-sidebar" />

    <div class="content-region">
      <main id="main" class="main-region" :aria-label="t('a11y.mainContent')">
        <button v-if="ui.sidebarCollapsed && ui.shellMode !== 'compact'" class="ui-icon-button sidebar-reopen" data-variant="ghost" data-size="md" type="button" :aria-label="t('sidebar.openMenu')" v-tooltip="t('sidebar.openMenu')" @click="ui.sidebarCollapsed = false">
          <PanelLeftOpen aria-hidden="true" />
        </button>
        <header class="main-header">
          <div><p>Dim {{ connection.hello?.serverVersion }}</p><strong>{{ pageTitle }}</strong></div>
        </header>
        <p v-if="connection.phase === 'reconnecting'" class="reconnect-banner" role="status">{{ t("page.reconnecting") }}</p>
        <RouterView :key="route.fullPath" />
      </main>
    </div>
  </div>
</template>
