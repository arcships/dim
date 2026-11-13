import { computed, ref, watch } from "vue"
import { defineStore } from "pinia"
import { approvalKeyboardBranch } from "../features/lines/line-state"
import { useAppearanceStore } from "./appearance"

export type ShellMode = "compact" | "regular" | "wide"

export type NewLineRequest = {
  sessionId: string
  parentBranchId: string
  at: number
  text: string
  before?: string
}

export type EndLineRequest = {
  sessionId: string
  branchId: string
  purpose: string
}

export type SidebarSortBy = "recentActive" | "recentCreated"

const STORAGE_KEY = "dim-web.ui.v1"
const DETAIL_MIN = 320
const DETAIL_MAX = 720

/** Layout state remembered in this browser. */
type ExtensionSelection = { pluginName: string; panelId: string }

type Persisted = {
  extension?: ExtensionSelection
  sidebarCollapsed: boolean
  collapsedWorkspaces: string[]
  lastSessionId: string | undefined
  detailOpen: boolean
  detailWidth: number
  sidebarSortBy: SidebarSortBy
}

/** localStorage is outside the app: read it once, keeping only well-formed fields. */
function readPersisted(): Persisted {
  const fallback: Persisted = { sidebarCollapsed: false, collapsedWorkspaces: [], lastSessionId: undefined, detailOpen: false, detailWidth: 420, sidebarSortBy: "recentActive" }
  try {
    const value: unknown = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null")
    if (value === null || typeof value !== "object") return fallback
    const stored = value as Record<string, unknown>
    const width = typeof stored.detailWidth === "number" && Number.isFinite(stored.detailWidth)
      ? Math.min(DETAIL_MAX, Math.max(DETAIL_MIN, stored.detailWidth))
      : fallback.detailWidth
    return {
      sidebarCollapsed: stored.sidebarCollapsed === true,
      collapsedWorkspaces: Array.isArray(stored.collapsedWorkspaces) ? stored.collapsedWorkspaces.filter((item): item is string => typeof item === "string") : [],
      lastSessionId: typeof stored.lastSessionId === "string" ? stored.lastSessionId : undefined,
      detailOpen: stored.detailOpen === true,
      extension: stored.extension !== null && typeof stored.extension === "object" && "pluginName" in stored.extension && "panelId" in stored.extension && typeof stored.extension.pluginName === "string" && typeof stored.extension.panelId === "string" ? { pluginName: stored.extension.pluginName, panelId: stored.extension.panelId } : undefined,
      detailWidth: width,
      sidebarSortBy: stored.sidebarSortBy === "recentCreated" ? "recentCreated" : "recentActive",
    }
  } catch {
    return fallback
  }
}

/** Layout persistence is best-effort: full or disabled localStorage is ignored. */
export const useUiStore = defineStore("ui", () => {
  const persisted = readPersisted()
  const sidebarCollapsed = ref(persisted.sidebarCollapsed)
  const collapsedWorkspaces = ref(persisted.collapsedWorkspaces)
  const lastSessionId = ref(persisted.lastSessionId)
  const drawerOpen = ref(false)
  const shellWidth = ref(typeof window !== "undefined" && window.innerWidth > 0 ? window.innerWidth : 1200)
  const shellMode = computed<ShellMode>(() => shellWidth.value < 720 ? "compact" : shellWidth.value < 960 ? "regular" : "wide")
  const extension = ref(persisted.extension)
  const detailOpen = ref(persisted.detailOpen)
  const detailWidth = ref(persisted.detailWidth)
  const sidebarSortBy = ref<SidebarSortBy>(persisted.sidebarSortBy)
  const detailExpanded = ref(false)
  const detailFocused = ref(false)
  const lineTabs = ref<Record<string, string[]>>({})
  const activeLineTab = ref<Record<string, string | undefined>>({})
  const newLine = ref<NewLineRequest | undefined>()
  const endingLine = ref<EndLineRequest | undefined>()

  watch([sidebarCollapsed, collapsedWorkspaces, lastSessionId, detailOpen, detailWidth, extension, sidebarSortBy], () => {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify({
        sidebarCollapsed: sidebarCollapsed.value,
        collapsedWorkspaces: collapsedWorkspaces.value,
        lastSessionId: lastSessionId.value,
        detailOpen: detailOpen.value,
        extension: extension.value,
        detailWidth: detailWidth.value,
        sidebarSortBy: sidebarSortBy.value,
      }))
    } catch {
    }
  }, { deep: true })

  useAppearanceStore()

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

  function setDetailWidth(width: number): void {
    detailWidth.value = Math.min(DETAIL_MAX, Math.max(DETAIL_MIN, Math.round(width)))
  }

  function openLines(sessionId: string): void {
    extension.value = undefined
    detailOpen.value = true
    detailFocused.value = true
    activeLineTab.value = { ...activeLineTab.value, [sessionId]: undefined }
  }

  function toggleLines(sessionId: string): void {
    if (detailOpen.value && extension.value === undefined && activeLineTab.value[sessionId] === undefined) {
      detailOpen.value = false
      detailFocused.value = false
    } else openLines(sessionId)
  }

  function openLine(sessionId: string, branchId: string): void {
    extension.value = undefined
    const tabs = lineTabs.value[sessionId] ?? []
    if (!tabs.includes(branchId)) lineTabs.value = { ...lineTabs.value, [sessionId]: [...tabs, branchId] }
    activeLineTab.value = { ...activeLineTab.value, [sessionId]: branchId }
    detailOpen.value = true
    detailFocused.value = true
  }

  function activateLine(sessionId: string, branchId: string): void {
    extension.value = undefined
    if (!(lineTabs.value[sessionId] ?? []).includes(branchId)) {
      openLine(sessionId, branchId)
      return
    }
    activeLineTab.value = { ...activeLineTab.value, [sessionId]: branchId }
    detailOpen.value = true
    detailFocused.value = true
  }

  function closeLineTab(sessionId: string, branchId: string): void {
    const tabs = (lineTabs.value[sessionId] ?? []).filter((id) => id !== branchId)
    lineTabs.value = { ...lineTabs.value, [sessionId]: tabs }
    if (activeLineTab.value[sessionId] === branchId) {
      activeLineTab.value = { ...activeLineTab.value, [sessionId]: tabs.at(-1) }
    }
  }

  function closeDetail(): void {
    detailOpen.value = false
    detailExpanded.value = false
    detailFocused.value = false
  }

  function approvalKeyboard(sessionId: string, branchId: string, approvals: readonly { branchId: string }[]): boolean {
    if (extension.value !== undefined && detailOpen.value && detailFocused.value) return false
    return approvalKeyboardBranch(approvals, {
      detailOpen: detailOpen.value,
      detailExpanded: detailExpanded.value,
      shellWidth: shellWidth.value,
      activeLine: activeLineTab.value[sessionId],
      detailFocused: detailFocused.value,
    }) === branchId
  }

  function requestNewLine(request: NewLineRequest): void {
    newLine.value = request
  }

  function closeNewLine(): void {
    newLine.value = undefined
  }

  function requestEndLine(request: EndLineRequest): void {
    endingLine.value = request
  }

  function closeEndLine(): void {
    endingLine.value = undefined
  }

  function resetSessionTabs(sessionId: string): void {
    lineTabs.value = { ...lineTabs.value, [sessionId]: [] }
    activeLineTab.value = { ...activeLineTab.value, [sessionId]: undefined }
    if (newLine.value?.sessionId === sessionId) newLine.value = undefined
    if (endingLine.value?.sessionId === sessionId) endingLine.value = undefined
    if (detailOpen.value) detailOpen.value = false
    detailFocused.value = false
  }

  return {
    sidebarCollapsed, lastSessionId, drawerOpen, shellWidth, shellMode, setShellWidth,
    isWorkspaceExpanded, toggleWorkspace, sidebarSortBy,
    extension, detailOpen, detailWidth, detailExpanded, detailFocused, lineTabs, activeLineTab, newLine, endingLine,
    setDetailWidth, openLines, toggleLines, openLine, activateLine, closeLineTab, closeDetail,
    approvalKeyboard,
    requestNewLine, closeNewLine, requestEndLine, closeEndLine, resetSessionTabs,
  }
})
