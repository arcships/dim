<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, onUpdated, provide, ref, shallowRef, toRef, watch } from "vue"
import { useI18n } from "vue-i18n"
import { useVirtualizer } from "@tanstack/vue-virtual"
import {
  CHAT_UI_HOST,
  PlainTimelineSurface,
  deriveTimelineUserMessageMarkers,
  shouldShowTimelineUserMessageRail,
  type ChatUiPlainTimelineSurfaceProps,
  type ChatUiPlainTimelineTurnUiState,
  type ChatUiTimelineUserMessageMarker,
} from "@dimcode/chat-ui"
import BrandMark from "../components/BrandMark.vue"
import ConfirmDialog from "../components/ConfirmDialog.vue"
import { retryAnnouncement, retrySeconds, retryStatus } from "../failures"
import type { TimelineRow } from "../projection"
import type { ModelRetryProgress } from "../rpc/protocol"
import { useToastStore } from "../stores/toast"
import { WebErrorBoundary, WebTimelineRow, WebUserMessageRail, createLabels } from "./components"
import { WEB_TIMELINE, icons } from "./primitives"
import { createRowMapper } from "./rows"
import { createWebChatUiHost } from "./web-host"

const props = defineProps<{
  sessionId?: string
  branchId: string
  earlier?: boolean
  loadingEarlier?: boolean
  historyError?: string
  rows: readonly TimelineRow[]
  running: boolean
  retry?: ModelRetryProgress
  quotable: boolean
  branchable?: boolean
  empty?: string
  variant?: "main" | "panel"
}>()
const emit = defineEmits<{ earlier: []; quote: [text: string]; branch: [text: string, cut: number]; openLine: [branchId: string] }>()
const { t } = useI18n()

const FOLLOW_THRESHOLD_PX = 96
const ROW_ESTIMATE_PX = 120

const mapRows = createRowMapper()
const rows = computed(() => mapRows(props.rows, props.sessionId ?? ""))
const sourceRows = computed(() => new Map(props.rows.map(row => [row.key, row])))
const rowByKey = (key: string): TimelineRow | undefined => sourceRows.value.get(key)

function noticeText(key: string): string {
  const row = rowByKey(key)
  switch (row?.kind) {
    case "line-ended":
      return row.issuer === "itself" ? t("timeline.lineEndedItself") : row.issuer === "operation" ? t("timeline.lineEndedByOperation") : t("timeline.lineEndedWith", { id: row.ancestor ?? "" })
    case "source-message":
      return `${t("timeline.fromLine", { id: row.from })}${row.steer ? ` ${t("timeline.steer")}` : ""}${row.final ? `, ${t("timeline.final")}` : ""}\n${row.body}`
    case "source-idle":
      return t("timeline.lineWaiting", { id: row.branch, status: row.status })
    case "source-ended":
      return t("timeline.lineEnded", { id: row.branch })
    default:
      return ""
  }
}

const isDark = ref(false)
let themeObserver: MutationObserver | undefined
function readTheme(): void {
  const root = document.documentElement
  isDark.value = root.dataset.theme === "dark" || root.classList.contains("dark")
}

const labels = computed(() => createLabels())
const toast = useToastStore()
const forkConfirm = ref<InstanceType<typeof ConfirmDialog>>()
const { host, actions } = createWebChatUiHost({
  sessionId: toRef(props, "sessionId"),
  branchId: toRef(props, "branchId"),
  rows,
  running: toRef(props, "running"),
  branchable: computed(() => props.branchable === true),
  rowByKey,
  noticeText,
  confirmFork: async text => await forkConfirm.value?.open(text, { confirmLabel: t("fork.confirmLabel") }) === true,
  openBranch: (text, cut) => emit("branch", text, cut),
})
/**
 * While the line runs, the user messages no turn end has settled: the running
 * turn's own, steered into it, or held for the next. A notice or error inside
 * the turn splits it into several display rows, so the renderer's latest-row
 * check alone does not find them.
 */
const runningUserMessageIds = computed(() => {
  const ids = new Set<string>()
  if (!props.running) return ids
  for (const row of props.rows) if (row.kind === "user" && !row.settled) ids.add(row.key)
  return ids
})
provide(CHAT_UI_HOST, host)
provide(WEB_TIMELINE, {
  runningUserMessageIds,
  rowByKey,
  isDark,
  labels,
  openLine: branchId => emit("openLine", branchId),
  actions,
  quotable: toRef(props, "quotable"),
  quote: text => emit("quote", text),
})

/** Runs a timeline action; a refusal or failure is shown, never swallowed. */
function act(command: Promise<void>): void {
  command.catch((cause: unknown) => toast.push({ tone: "danger", title: cause instanceof Error ? cause.message : String(cause) }))
}

/** The surface types its `fork-message` payload `unknown`; the row it forwards sends the forked message's id. */
function forkMessage(message: unknown): void {
  if (typeof message === "object" && message !== null && "id" in message && typeof message.id === "string") act(host.commands.fork(message.id))
}

const shell = ref<HTMLElement>()
const railHost = ref<HTMLElement | null>(null)
const surface = shallowRef<InstanceType<typeof PlainTimelineSurface>>()
const viewport = computed(() => surface.value?.viewport ?? null)
const scrollMargin = ref(0)
const following = ref(true)

const virtualizer = useVirtualizer(computed(() => ({
  count: rows.value.length,
  getScrollElement: () => viewport.value,
  estimateSize: () => ROW_ESTIMATE_PX,
  overscan: 4,
  getItemKey: (index: number) => rows.value[index]!.key,
  indexAttribute: "data-timeline-row-index",
  scrollMargin: scrollMargin.value,
})))
// Rows are in normal flow inside the surface; the viewport's scroll anchoring is
// off, so the virtualizer alone keeps the reader in place. A resized row moves
// the view only when it sits wholly above the viewport, judged by its current
// start: TanStack rebuilds measurementsCache lazily, and the item it passes can
// carry a stale estimated start. While a prepend's reading position is held,
// every row above the held row moves the view, measured or partly visible.
// During Return to latest none does: its smooth scroll aims at the end, and an
// adjustment would be an instant scroll that ends it.
let heldIndex: number | undefined
virtualizer.value.shouldAdjustScrollPositionOnItemSizeChange = (item, _delta, instance) => {
  const element = viewport.value
  if (element === null || jumpResumed) return false
  if (heldIndex !== undefined && item.index < heldIndex) return true
  instance.getTotalSize()
  const start = instance.measurementsCache[item.index]?.start ?? item.start
  return start + item.size <= element.scrollTop
}

const surfaceWindow = computed<ChatUiPlainTimelineSurfaceProps["window"]>(() => {
  const items = virtualizer.value.getVirtualItems()
  const first = items[0]
  const last = items.at(-1)
  const start = first?.index ?? 0
  const end = last === undefined ? 0 : last.index + 1
  return {
    start,
    end,
    rows: rows.value.slice(start, end),
    topSpacerPx: first === undefined ? 0 : first.start - scrollMargin.value,
    bottomSpacerPx: last === undefined ? 0 : Math.max(0, virtualizer.value.getTotalSize() - (last.end - scrollMargin.value)),
    contentVisibilityEnabled: () => false,
    contentVisibilityStyle: () => undefined,
  }
})

function measureRows(): void {
  const element = viewport.value
  const content = surface.value?.content
  if (element === null || content == null) return
  const margin = content.getBoundingClientRect().top - element.getBoundingClientRect().top + element.scrollTop
  if (Math.abs(margin - scrollMargin.value) > 0.5) scrollMargin.value = margin
  for (const row of content.querySelectorAll<HTMLElement>(":scope > [data-timeline-row-index]")) virtualizer.value.measureElement(row)
}

const quoteTarget = ref<{ text: string; top: number; left: number }>()

let jumpResumed = false
let jumpTop = 0

function onScroll(): void {
  const element = viewport.value
  if (element === null) return
  const distance = element.scrollHeight - element.scrollTop - element.clientHeight
  if (distance <= 1 || element.scrollTop < jumpTop - 1) jumpResumed = false
  jumpTop = element.scrollTop
  following.value = jumpResumed || distance <= FOLLOW_THRESHOLD_PX
  quoteTarget.value = undefined
  autoLoadEarlier()
}

function onScrollEnd(): void {
  jumpResumed = false
  onScroll()
}

function onReaderScroll(): void {
  jumpResumed = false
  heldIndex = undefined
}

const READER_SCROLL_EVENTS = ["wheel", "touchstart", "pointerdown", "keydown"] as const

watch(viewport, (element, previous) => {
  previous?.removeEventListener("scroll", onScroll)
  previous?.removeEventListener("scrollend", onScrollEnd)
  for (const name of READER_SCROLL_EVENTS) previous?.removeEventListener(name, onReaderScroll)
  element?.addEventListener("scroll", onScroll, { passive: true })
  element?.addEventListener("scrollend", onScrollEnd, { passive: true })
  for (const name of READER_SCROLL_EVENTS) element?.addEventListener(name, onReaderScroll, { passive: true })
})

async function toEnd(): Promise<void> {
  await nextTick()
  const element = viewport.value
  if (!following.value || element === null) return
  if (jumpResumed) element.scrollTo({ top: element.scrollHeight, behavior: "smooth" })
  else element.scrollTop = element.scrollHeight
}

const unseen = ref(false)
watch(following, (value) => {
  if (value) {
    unseen.value = false
    heldIndex = undefined
  } else jumpResumed = false
})

function jumpToLatest(): void {
  const element = viewport.value
  if (element === null) return
  jumpResumed = true
  jumpTop = element.scrollTop
  following.value = true
  element.scrollTo({ top: element.scrollHeight, behavior: "smooth" })
}

watch([() => rows.value.length, () => virtualizer.value.getTotalSize(), () => props.rows.at(-1), () => props.running], () => {
  if (following.value) void toEnd()
})
watch(() => props.rows, (next, previous) => {
  if (following.value || unseen.value || previous.length === 0) return
  const shown = new Map(previous.map(row => [row.key, row]))
  const lastIndex = next.findIndex(row => row.key === previous.at(-1)?.key)
  unseen.value = next.some((row, index) => shown.has(row.key) ? shown.get(row.key) !== row : index > lastIndex)
})

let earlierPending = false
function loadEarlier(): void {
  if (props.loadingEarlier === true || earlierPending) return
  earlierPending = true
  following.value = false
  emit("earlier")
  void nextTick(() => { earlierPending = false })
}

let earlierVisible = false
function autoLoadEarlier(): void {
  if (earlierVisible && !following.value && !props.historyError) loadEarlier()
}

const earlierRow = ref<HTMLElement | null>(null)
let earlierObserver: IntersectionObserver | undefined
watch([earlierRow, viewport, () => props.loadingEarlier], ([row, root]) => {
  earlierObserver?.disconnect()
  earlierObserver = undefined
  earlierVisible = false
  if (row === null || root === null) return
  earlierObserver = new IntersectionObserver((entries) => {
    earlierVisible = entries.at(-1)!.isIntersecting
    autoLoadEarlier()
  }, { root })
  earlierObserver.observe(row)
}, { flush: "post" })

watch(() => rows.value[0]?.key, async (first, previous) => {
  const element = viewport.value
  const content = surface.value?.content
  if (previous === undefined || first === previous || following.value || element === null || content == null) return
  const keys = new Set(rows.value.map(row => row.key))
  const top = element.getBoundingClientRect().top
  let anchor: { key: string; top: number } | undefined
  for (const row of content.querySelectorAll<HTMLElement>(":scope > [data-timeline-row-key]")) {
    const rect = row.getBoundingClientRect()
    if (rect.bottom > top && keys.has(row.dataset.timelineRowKey!)) {
      anchor = { key: row.dataset.timelineRowKey!, top: rect.top - top }
      break
    }
  }
  if (anchor === undefined) return
  await nextTick()
  const index = rows.value.findIndex(row => row.key === anchor.key)
  virtualizer.value.getTotalSize()
  const start = virtualizer.value.measurementsCache[index]?.start
  if (start === undefined) return
  heldIndex = index
  element.scrollTop = start - anchor.top
}, { flush: "pre" })

const markers = computed(() => deriveTimelineUserMessageMarkers(rows.value))
const showRail = computed(() => props.variant !== "panel" && shouldShowTimelineUserMessageRail(markers.value))

function jumpTo(marker: ChatUiTimelineUserMessageMarker): void {
  const index = rows.value.findIndex(row => row.key === marker.rowKey)
  if (index < 0) return
  following.value = false
  heldIndex = undefined
  virtualizer.value.scrollToIndex(index, { align: "start" })
}

function onRailWheel(event: WheelEvent): void {
  jumpResumed = false
  heldIndex = undefined
  viewport.value?.scrollBy({ top: event.deltaY })
}

const turnUi = ref<Record<string, ChatUiPlainTimelineTurnUiState>>({})
const blockExpanded = ref<Record<string, boolean>>({})
const groupExpanded = ref<Record<string, boolean>>({})

function toggleDetails(turnId: string): void {
  following.value = false
  turnUi.value[turnId] = { detailsExpanded: turnUi.value[turnId]?.detailsExpanded !== true, detailsToggleVisible: true }
}

function toggle(expanded: Record<string, boolean>, id: string): void {
  following.value = false
  expanded[id] = expanded[id] !== true
}

/** The client clock the retry countdown reads, ticked while a retry is set. */
const now = ref(Date.now())
let countdown: ReturnType<typeof setInterval> | undefined
watch(() => props.retry, (retry) => {
  clearInterval(countdown)
  countdown = undefined
  if (retry === undefined) return
  now.value = Date.now()
  countdown = setInterval(() => { now.value = Date.now() }, 500)
}, { immediate: true })

const NO_USER_MESSAGES = new Map<string, unknown>()

const surfaceProps = computed<Omit<ChatUiPlainTimelineSurfaceProps, "window">>(() => {
  let latestTurn: string | null = null
  for (const row of rows.value) if (row.kind === "turn") latestTurn = row.turn.id
  return {
    state: {
      sessionId: props.sessionId ?? null,
      messageCount: rows.value.length,
      rowCount: rows.value.length,
      isRunning: props.running,
      isLoadingOlder: props.loadingEarlier === true,
      restoreCovered: false,
      restoreCoverVisible: false,
      restoreTransitionSuppressed: false,
      layoutResizing: false,
      canLoadOlder: props.earlier === true,
      hasNewerGap: false,
      showUserMessageRail: showRail.value,
      userMessageRailHost: railHost.value,
      showScrollToBottom: false,
    },
    rowState: {
      latestTurnRowId: latestTurn,
      turnUiStateById: turnUi.value,
      blockExpandedById: blockExpanded.value,
      blockGroupExpandedById: groupExpanded.value,
      assistantUserMessageIdByRowKey: {},
      userMessageById: NO_USER_MESSAGES,
      searchQuery: "",
      activeSearchMatchId: null,
      searchMatchesBySource: {},
      elapsedNowMs: undefined,
      presentationActive: true,
      readOnly: !host.capabilities.value.interactive,
    },
    diagnostics: {
      prependAnchor: null,
      oldestRawMessageId: undefined,
      trimState: "idle",
      trimAnchorKey: "",
      trimAtBottom: false,
      trimRestoreState: "idle",
      trimCapturedTop: null,
      trimRestoredTop: null,
      renderWindowStart: surfaceWindow.value.start,
      renderWindowEnd: surfaceWindow.value.end,
    },
    markers: {
      markers: markers.value,
      loadedMarkers: markers.value,
      rowDomSignature: `${surfaceWindow.value.start}:${surfaceWindow.value.end}`,
    },
    // The retry countdown is not a live region (docs/design/model-failures.md §3).
    activity: {
      visible: props.running && props.retry === undefined,
      text: t("work.processing"),
    },
    labels: {
      emptySession: props.empty ?? t("chat.emptyTimeline"),
      loading: t("common.loading"),
      newerHistoryGap: t("chatUi.timeline.newerHistoryGap"),
      returnToLatest: t("chatUi.timeline.returnToLatest"),
    },
    components: {
      row: WebTimelineRow,
      errorBoundary: WebErrorBoundary,
      userMessageRail: WebUserMessageRail,
      brandMark: BrandMark,
      chevronDown: icons.chevronDown,
      chevronUp: icons.chevronUp,
    },
  }
})

function onPointerUp(): void {
  const selection = window.getSelection()
  const root = shell.value
  const text = selection?.toString() ?? ""
  if (!props.quotable || selection === null || selection.isCollapsed || root === undefined
    || !root.contains(selection.anchorNode) || !root.contains(selection.focusNode) || text.trim() === "") {
    quoteTarget.value = undefined
    return
  }
  const rect = selection.getRangeAt(0).getBoundingClientRect()
  const box = root.getBoundingClientRect()
  quoteTarget.value = { text, top: rect.top - box.top, left: rect.left - box.left + rect.width / 2 }
}

function quoteSelection(): void {
  if (quoteTarget.value === undefined) return
  emit("quote", quoteTarget.value.text)
  window.getSelection()?.removeAllRanges()
  quoteTarget.value = undefined
}

onMounted(() => {
  readTheme()
  themeObserver = new MutationObserver(readTheme)
  themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "data-theme"] })
  measureRows()
  findActivitySlot()
  void toEnd()
})
/** The surface's activity line, which the visible retry countdown fills. */
const activitySlot = shallowRef<HTMLElement | null>(null)
function findActivitySlot(): void {
  const element = surface.value?.flow?.querySelector<HTMLElement>("[data-timeline-activity-slot]") ?? null
  if (element !== activitySlot.value) activitySlot.value = element
}
const retryText = computed(() => props.retry === undefined ? "" : retryStatus(props.retry, retrySeconds(props.retry.retryAtMs, now.value)))
const retryLive = computed(() => retryAnnouncement(props.retry))

onUpdated(() => {
  measureRows()
  findActivitySlot()
})
onBeforeUnmount(() => {
  themeObserver?.disconnect()
  clearInterval(countdown)
  viewport.value?.removeEventListener("scroll", onScroll)
  viewport.value?.removeEventListener("scrollend", onScrollEnd)
  for (const name of READER_SCROLL_EVENTS) viewport.value?.removeEventListener(name, onReaderScroll)
  earlierObserver?.disconnect()
})
</script>

<template>
  <div ref="shell" class="timeline-shell web-chat-timeline dim-chat-ui" :data-variant="variant ?? 'main'" @pointerup="onPointerUp">
    <PlainTimelineSurface
      ref="surface"
      v-bind="surfaceProps"
      :window="surfaceWindow"
      @toggle-details="toggleDetails"
      @toggle-block="toggle(blockExpanded, $event)"
      @toggle-block-group="toggle(groupExpanded, $event)"
      @marker-click="jumpTo"
      @rail-wheel="onRailWheel"
      @fork-message="forkMessage"
      @branch-message="act(host.commands.branch($event.id))"
    >
      <template #before-rows>
        <div v-if="earlier" ref="earlierRow" class="web-chat-earlier" data-testid="web-chat-earlier">
          <span v-if="loadingEarlier" class="web-chat-earlier-status" role="status">
            <UIcon name="i-lucide-loader-circle" class="web-chat-earlier-icon animate-spin" aria-hidden="true" />{{ t("timeline.loadingEarlier") }}
          </span>
          <span v-else-if="historyError" class="web-chat-earlier-status" role="alert">
            <span class="web-chat-earlier-error">{{ historyError }}</span>
            <button type="button" class="web-chat-earlier-action" @click="loadEarlier">{{ t("timeline.retryEarlier") }}</button>
          </span>
          <button v-else type="button" class="web-chat-earlier-action" @click="loadEarlier">
            <UIcon name="i-lucide-arrow-up" class="web-chat-earlier-icon" aria-hidden="true" />{{ t("timeline.loadEarlier") }}
          </button>
        </div>
      </template>
    </PlainTimelineSurface>
    <Transition name="web-chat-latest">
      <button v-if="!following" type="button" class="web-chat-latest" data-testid="web-chat-jump-latest" @click="jumpToLatest">
        <UIcon name="i-lucide-arrow-down" class="web-chat-latest-icon" aria-hidden="true" />
        <span v-if="unseen" class="web-chat-latest-new">{{ t("timeline.newMessages") }}</span>
        <span>{{ t("chatUi.timeline.returnToLatest") }}</span>
      </button>
    </Transition>
    <Teleport v-if="running && retry !== undefined && activitySlot" :to="activitySlot">
      <span class="tabular-nums" aria-hidden="true" data-testid="web-chat-retry">{{ retryText }}</span>
    </Teleport>
    <span class="sr-only" role="status" aria-live="polite">{{ running ? retryLive : "" }}</span>
    <div ref="railHost" class="web-chat-rail" />
    <UButton
      v-if="quoteTarget"
      class="web-chat-quote"
      :style="{ top: `${quoteTarget.top}px`, left: `${quoteTarget.left}px` }"
      color="neutral"
      variant="solid"
      size="xs"
      icon="i-lucide-quote"
      @pointerdown.prevent
      @click="quoteSelection"
    >{{ t("message.quote") }}</UButton>
    <ConfirmDialog ref="forkConfirm" />
  </div>
</template>

<style scoped>
.web-chat-timeline { flex: 1 1 auto; }
.web-chat-timeline :deep(.chat-scroll-viewport) { overflow-anchor: none; }
.web-chat-earlier { display: flex; align-items: center; gap: var(--space-3); padding: 0 var(--space-4) var(--space-4); color: var(--muted-foreground); font-size: var(--text-sm); }
.web-chat-earlier::before, .web-chat-earlier::after { content: ""; flex: 1 1 0; min-width: var(--space-4); border-top: 1px solid var(--border); }
.web-chat-earlier-status { display: inline-flex; min-width: 0; align-items: center; gap: var(--space-2); }
.web-chat-earlier-error { min-width: 0; overflow: hidden; color: var(--danger); text-overflow: ellipsis; white-space: nowrap; }
.web-chat-earlier-action { display: inline-flex; flex: none; align-items: center; gap: var(--space-1); padding: var(--space-1) var(--space-2); border: 0; border-radius: var(--control-xs-radius); background: none; color: inherit; font: inherit; cursor: pointer; transition: color var(--duration-hover) var(--ease-out), background-color var(--duration-hover) var(--ease-out); }
.web-chat-earlier-action:hover { background: var(--accent); color: var(--foreground); }
.web-chat-earlier-action:focus-visible { outline: 2px solid var(--ring); outline-offset: 1px; }
.web-chat-earlier-icon { width: 14px; height: 14px; flex: none; }
.web-chat-latest { position: absolute; bottom: var(--space-4); left: 50%; z-index: 30; display: inline-flex; align-items: center; gap: var(--space-2); height: var(--control-md); padding: 0 var(--space-3); transform: translateX(-50%); border: 1px solid var(--border); border-radius: var(--radius-pill); background: var(--background); box-shadow: var(--shadow-sm); color: var(--foreground); font: inherit; font-size: var(--text-sm); font-weight: var(--weight-medium); white-space: nowrap; cursor: pointer; transition: background-color var(--duration-hover) var(--ease-out); }
.web-chat-latest:hover { background: var(--accent); }
.web-chat-latest:focus-visible { outline: 2px solid var(--ring); outline-offset: 2px; }
.web-chat-latest-icon { width: 14px; height: 14px; flex: none; color: var(--muted-foreground); }
.web-chat-latest-new { color: var(--primary); }
.web-chat-timeline[data-variant="panel"] .web-chat-latest { bottom: var(--space-3); height: var(--control-sm); padding: 0 var(--space-2); font-size: var(--text-xs); }
.web-chat-latest-enter-active, .web-chat-latest-leave-active { transition: opacity var(--duration-hover) var(--ease-out); }
.web-chat-latest-enter-from, .web-chat-latest-leave-to { opacity: 0; }
.web-chat-rail { position: absolute; top: var(--space-4); right: var(--space-2); z-index: 2; }
.web-chat-quote { position: absolute; z-index: 40; transform: translate(-50%, calc(-100% - 6px)); }
</style>
