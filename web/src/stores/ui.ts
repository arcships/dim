import { computed, ref, watch } from "vue"
import { defineStore } from "pinia"

export type ShellMode = "compact" | "regular" | "wide"

const STORAGE_KEY = "dim-web.ui.v1"

/** Layout state remembered in this browser. */
type Persisted = {
  sidebarCollapsed: boolean
  collapsedWorkspaces: string[]
  lastSessionId: string | undefined
}

/** localStorage is outside the app: read it once, keeping only well-formed fields. */
function readPersisted(): Persisted {
  const fallback: Persisted = { sidebarCollapsed: false, collapsedWorkspaces: [], lastSessionId: undefined }
  try {
    const value: unknown = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null")
    if (value === null || typeof value !== "object") return fallback
    const stored = value as Record<string, unknown>
    return {
      sidebarCollapsed: stored.sidebarCollapsed === true,
      collapsedWorkspaces: Array.isArray(stored.collapsedWorkspaces) ? stored.collapsedWorkspaces.filter((item): item is string => typeof item === "string") : [],
      lastSessionId: typeof stored.lastSessionId === "string" ? stored.lastSessionId : undefined,
    }
  } catch {
    return fallback
  }
}

export const useUiStore = defineStore("ui", () => {
  const persisted = readPersisted()
  const sidebarCollapsed = ref(persisted.sidebarCollapsed)
  const collapsedWorkspaces = ref(persisted.collapsedWorkspaces)
  const lastSessionId = ref(persisted.lastSessionId)
  const drawerOpen = ref(false)
  const shellWidth = ref(960)
  const shellMode = computed<ShellMode>(() => shellWidth.value < 720 ? "compact" : shellWidth.value < 960 ? "regular" : "wide")

  watch([sidebarCollapsed, collapsedWorkspaces, lastSessionId], () => {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify({
        sidebarCollapsed: sidebarCollapsed.value,
        collapsedWorkspaces: collapsedWorkspaces.value,
        lastSessionId: lastSessionId.value,
      }))
    } catch {
      // Storage is full or disabled; the layout is simply not remembered.
    }
  }, { deep: true })

  // The page follows the system color scheme; global.css keys dark tokens on `.dark`.
  const systemDark = matchMedia("(prefers-color-scheme: dark)")
  const applyTheme = (): void => { document.documentElement.classList.toggle("dark", systemDark.matches) }
  applyTheme()
  systemDark.addEventListener("change", applyTheme)

  function setShellWidth(width: number): void {
    shellWidth.value = width
    if (shellMode.value !== "compact") drawerOpen.value = false
  }

  function isWorkspaceExpanded(cwd: string): boolean {
    return !collapsedWorkspaces.value.includes(cwd)
  }

  function toggleWorkspace(cwd: string): void {
    collapsedWorkspaces.value = isWorkspaceExpanded(cwd)
      ? [...collapsedWorkspaces.value, cwd]
      : collapsedWorkspaces.value.filter((item) => item !== cwd)
  }

  return { sidebarCollapsed, lastSessionId, drawerOpen, shellWidth, shellMode, setShellWidth, isWorkspaceExpanded, toggleWorkspace }
})
