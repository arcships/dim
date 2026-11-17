<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch, type ComponentPublicInstance } from "vue"
import { useI18n } from "vue-i18n"
import { useVirtualizer } from "@tanstack/vue-virtual"
import type { TimelineRow } from "../../projection"
import ChatMarkdown from "./ChatMarkdown.vue"
import TimelineUserMessage from "./TimelineUserMessage.vue"
import TimelineWorkRow from "./TimelineWorkRow.vue"
import { groupRows } from "./work"
import { failureDetails, retryAnnouncement, retrySeconds, retryStatus } from "../../failures"
import type { ModelRetryProgress } from "../../rpc/protocol"
import { RAIL_MIN_MARKERS, timelineMarkers } from "../../timeline-markers"

/**
 * `quotable`: the composer is shown, so a message can be quoted into it.
 * `branchable`: the shown line is open, so a user message can open a child line.
 * `variant`: the main column or the narrower line panel.
 */
const props = defineProps<{
  sessionId?: string
  earlier?: boolean
  loadingEarlier?: boolean
  historyError?: string
  rows: readonly TimelineRow[]
  running: boolean
  /** The retry the running request is waiting to make, while it sleeps. */
  retry?: ModelRetryProgress
  quotable: boolean
  branchable?: boolean
  empty?: string
  variant?: "main" | "panel"
}>()
const emit = defineEmits<{ earlier: []; quote: [text: string]; branch: [text: string, cut: number]; openLine: [branchId: string] }>()
const { t } = useI18n()
const viewport = ref<HTMLElement>()
const list = ref<HTMLElement>()
/** The view follows new output while the reader is within this distance of the end. */
const FOLLOW_THRESHOLD_PX = 96
const following = ref(true)
const expandedGroups = ref<Record<string, boolean>>({})
const expandedItems = ref<Record<string, boolean>>({})

const display = computed(() => groupRows(props.rows))
const expandedDetails = ref<Record<string, boolean>>({})
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
onBeforeUnmount(() => clearInterval(countdown))

const retryText = computed(() => props.retry === undefined ? "" : retryStatus(props.retry, retrySeconds(props.retry.retryAtMs, now.value)))
/** The live region text: it follows the attempt, not the visible countdown. */
const retryLive = computed(() => retryAnnouncement(props.retry))

function toggleDetails(key: string): void {
  following.value = false
  expandedDetails.value[key] = expandedDetails.value[key] !== true
}

function toggleWork(expanded: Record<string, boolean>, key: string): void {
  following.value = false
  expanded[key] = expanded[key] !== true
}

function shortId(id: string): string {
  return id.slice(0, 8)
}

function onBranch(row: TimelineRow, text: string): void {
  if (row.kind === "user") emit("branch", text, row.cut)
}

const virtualizer = useVirtualizer(computed(() => ({
  count: display.value.length,
  getScrollElement: () => viewport.value ?? null,
  estimateSize: () => 72,
  overscan: 6,
  getItemKey: (index: number) => display.value[index]!.key,
})))
// TanStack decides whether a resized row sits above the reader from
// measurementsCache, which it rebuilds only on the next getMeasurements, against
// an offset that already carries this batch's corrections. With several rows
// measured in one batch, a row above the reader was judged by its stale
// estimated start, read as below, and its growth pushed the view down. Judge it
// by its current start against where the viewport actually is.
virtualizer.value.shouldAdjustScrollPositionOnItemSizeChange = (item, _delta, instance) => {
  const element = viewport.value
  if (element === undefined || list.value === undefined) return false
  instance.getTotalSize()
  const start = instance.measurementsCache[item.index]?.start ?? item.start
  // The list sits below the viewport's top padding; offsetTop is relative to the
  // positioned viewport.
  return start + item.size <= element.scrollTop - list.value.offsetTop
}
const virtualRows = computed(() => virtualizer.value.getVirtualItems())
const totalSize = computed(() => virtualizer.value.getTotalSize())

/** The rail's markers, one per user message; the rail lists them top to bottom. */
const markers = computed(() => timelineMarkers(display.value))
const showRail = computed(() => props.variant !== "panel" && markers.value.length >= RAIL_MIN_MARKERS)
const RAIL_HEIGHT_PX = 420
/** Markers stay within the rail height; a long history packs them closer. */
const railPitch = computed(() => Math.min(12, (RAIL_HEIGHT_PX - 12) / Math.max(1, markers.value.length - 1)))
const jumpTarget = ref<number>()
let jumpTimer: ReturnType<typeof setTimeout> | undefined

/** Scrolls the message at `index` into the middle of the view and marks it for a moment. */
function jumpTo(index: number): void {
  following.value = false
  virtualizer.value.scrollToIndex(index, { align: "center", behavior: "smooth" })
  jumpTarget.value = index
  clearTimeout(jumpTimer)
  jumpTimer = setTimeout(() => { jumpTarget.value = undefined }, 1600)
}

function measure(element: Element | ComponentPublicInstance | null): void {
  if (element instanceof Element) virtualizer.value.measureElement(element)
}

function onScroll(): void {
  const element = viewport.value
  if (element === undefined) return
  following.value = element.scrollHeight - element.scrollTop - element.clientHeight <= FOLLOW_THRESHOLD_PX
}

async function toEnd(): Promise<void> {
  await nextTick()
  const element = viewport.value
  if (following.value && element !== undefined) element.scrollTop = element.scrollHeight
}

function jumpToLatest(): void {
  following.value = true
  void toEnd()
}

// New rows and rows that grow as they stream keep the end in view while following.
watch([() => display.value.length, totalSize, () => props.rows.at(-1), () => props.running], () => {
  if (following.value) void toEnd()
})
onMounted(() => void toEnd())
</script>

<template>
  <div class="timeline-shell" :data-variant="variant ?? 'main'">
    <div ref="viewport" class="timeline app-scroll-viewport" @scroll.passive="onScroll">
      <UButton v-if="earlier" color="neutral" variant="soft" size="sm" :loading="loadingEarlier" :disabled="loadingEarlier" @click="following = false; emit('earlier')">{{ loadingEarlier ? t("timeline.loadingEarlier") : t("timeline.loadEarlier") }}</UButton>
      <p v-if="historyError" role="alert">{{ historyError }}</p>
      <p v-if="display.length === 0 && !running" class="timeline-empty">{{ empty ?? t("chat.emptyTimeline") }}</p>
      <div ref="list" class="virtual-timeline" :style="{ height: `${totalSize}px`, position: 'relative' }">
        <div
          v-for="item in virtualRows"
          :key="String(item.key)"
          :ref="measure"
          class="virtual-timeline-row"
          :class="{ 'is-jump-target': item.index === jumpTarget }"
          :data-index="item.index"
          :style="{ position: 'absolute', top: 0, left: 0, width: '100%', transform: `translateY(${item.start}px)` }"
        >
          <template v-for="entry in [display[item.index]!]" :key="entry.key">
            <TimelineWorkRow
              :session-id="sessionId"
              v-if="entry.kind === 'work'"
              :items="entry.items"
              :expanded="expandedGroups[entry.key] === true"
              :expanded-item-ids="expandedItems"
              @toggle="toggleWork(expandedGroups, entry.key)"
              @toggle-item="toggleWork(expandedItems, $event)"
            />
            <TimelineUserMessage v-else-if="entry.row.kind === 'user'" :text="entry.row.text" :images="entry.row.images" :session-id="sessionId" :skills="entry.row.skills" :hooks="entry.row.hooks" :at="entry.row.at" :quotable="quotable" :branchable="branchable === true" @branch="onBranch(entry.row, $event)" @quote="emit('quote', $event)" />
            <article v-else-if="entry.row.kind === 'assistant'" class="timeline-row" data-role="assistant">
              <strong class="sr-only">{{ t("chat.assistant") }}</strong>
              <ChatMarkdown :content="entry.row.text" :final="!entry.row.streaming" :message-id="entry.row.key" />
            </article>
            <button
              v-else-if="entry.row.kind === 'line'"
              type="button"
              class="timeline-line-marker"
              :disabled="entry.row.failed === true"
              :aria-label="t('lines.openLine', { name: entry.row.purpose || shortId(entry.row.branchId) })"
              @click="entry.row.failed !== true && emit('openLine', entry.row.branchId)"
            >
              <UIcon name="i-lucide-git-branch" class="size-3.5 shrink-0" aria-hidden="true" />
              <span class="timeline-line-name">{{ entry.row.purpose || shortId(entry.row.branchId) }}</span>
              <code>{{ shortId(entry.row.branchId) }}</code>
              <span v-if="entry.row.failed" class="timeline-line-failed">{{ t("lines.state.creationFailed") }}</span>
              <span v-else-if="entry.row.before" class="timeline-line-cut">{{ t("timeline.beforeMessage", { message: entry.row.before }) }}</span>
            </button>
            <p v-else-if="entry.row.kind === 'line-ended'" class="timeline-notice">
              {{ entry.row.issuer === "itself" ? t("timeline.lineEndedItself") : entry.row.issuer === "operation" ? t("timeline.lineEndedByOperation") : t("timeline.lineEndedWith", { id: entry.row.ancestor ?? "" }) }}
            </p>
            <p v-else-if="entry.row.kind === 'source-message'" class="timeline-line-notice">
              <span class="timeline-line-from">{{ t("timeline.fromLine", { id: entry.row.from }) }}<template v-if="entry.row.steer"> {{ t("timeline.steer") }}</template><template v-if="entry.row.final">, {{ t("timeline.final") }}</template></span>
              <span>{{ entry.row.body }}</span>
            </p>
            <p v-else-if="entry.row.kind === 'source-idle'" class="timeline-line-notice">{{ t("timeline.lineWaiting", { id: entry.row.branch, status: entry.row.status }) }}</p>
            <p v-else-if="entry.row.kind === 'source-ended'" class="timeline-line-notice">{{ t("timeline.lineEnded", { id: entry.row.branch }) }}</p>
            <div v-else-if="entry.row.kind === 'hook-follow-up'" class="timeline-hook">
              <button type="button" class="timeline-hook-row" :aria-expanded="expandedDetails[entry.row.key] === true" @click="toggleDetails(entry.row.key)">
                <span>{{ t("timeline.hookFollowUp", { count: entry.row.lines }, entry.row.lines) }}</span>
                <UIcon name="i-lucide-chevron-down" class="size-3.5 timeline-hook-chevron" :class="{ 'is-expanded': expandedDetails[entry.row.key] === true }" aria-hidden="true" />
              </button>
              <div v-if="expandedDetails[entry.row.key]" class="timeline-hook-body">
                <div v-for="(line, index) in entry.row.body.split('\n')" :key="index">{{ line }}</div>
              </div>
            </div>
            <div v-else-if="entry.row.kind === 'error'" class="notice-row error-row" role="alert">
              <span>{{ entry.row.text }}</span>
              <template v-if="entry.row.details">
                <button type="button" class="error-details-toggle" :aria-expanded="expandedDetails[entry.row.key] === true" @click="toggleDetails(entry.row.key)">{{ t("failure.details") }}</button>
                <div v-if="expandedDetails[entry.row.key]" class="error-details">
                  <div v-for="(line, index) in failureDetails(entry.row.details)" :key="index">{{ line }}</div>
                </div>
              </template>
            </div>
            <p v-else class="timeline-notice">{{ t("timeline.noticeStopped") }}</p>
          </template>
        </div>
      </div>
      <div v-if="running" class="processing-row" role="status">
        <UIcon name="i-lucide-loader-circle" class="size-3.5 animate-spin shrink-0" aria-hidden="true" />
        <span :aria-hidden="retry === undefined ? false : 'true'">{{ retry === undefined ? t("work.processing") : retryText }}</span>
        <span class="sr-only">{{ retryLive }}</span>
      </div>
    </div>
    <nav v-if="showRail" class="timeline-rail" :aria-label="t('timeline.rail')">
      <button
        v-for="(marker, position) in markers"
        :key="marker.key"
        type="button"
        class="timeline-marker"
        :style="{ top: `${position * railPitch}px` }"
        :aria-label="marker.prompt"
        @click="jumpTo(marker.index)"
      >
        <span class="timeline-marker-card">
          <span class="timeline-marker-prompt">{{ marker.prompt }}</span>
          <span v-if="marker.reply" class="timeline-marker-reply">{{ marker.reply }}</span>
        </span>
      </button>
    </nav>
    <Transition name="jump-latest">
      <div v-if="!following" class="jump-latest">
        <UTooltip :text="t('timeline.scrollToBottom')">
          <UButton color="neutral" variant="soft" size="md" square icon="i-lucide-chevron-down" :aria-label="t('timeline.scrollToBottom')" @click="jumpToLatest" />
        </UTooltip>
      </div>
    </Transition>
  </div>
</template>

<style scoped>
.timeline-rail { position: absolute; top: var(--space-4); right: var(--space-2); z-index: 2; width: 26px; height: 420px; }
.timeline-marker { position: absolute; right: 0; width: 14px; height: 2px; padding: 0; border: 0; border-radius: 1px; background: var(--muted-foreground); opacity: 0.45; cursor: pointer; }
.timeline-marker:hover, .timeline-marker:focus-visible { width: 22px; background: var(--foreground); opacity: 1; }
.timeline-marker-card { position: absolute; top: -10px; right: 30px; display: none; width: 280px; max-height: 160px; overflow: hidden; padding: var(--space-2) var(--space-3); border: 1px solid var(--border); border-radius: var(--radius-lg); background: var(--card); box-shadow: var(--shadow-sm); font-size: var(--text-xs); text-align: left; }
.timeline-marker:hover .timeline-marker-card, .timeline-marker:focus-visible .timeline-marker-card { display: grid; gap: var(--space-1); }
.timeline-marker-prompt { display: -webkit-box; overflow: hidden; -webkit-line-clamp: 3; -webkit-box-orient: vertical; color: var(--foreground); }
.timeline-marker-reply { display: -webkit-box; overflow: hidden; -webkit-line-clamp: 2; -webkit-box-orient: vertical; color: var(--muted-foreground); }
.virtual-timeline-row.is-jump-target :deep(.user-bubble) { box-shadow: var(--focus-ring); transition: box-shadow var(--duration-popover) var(--ease-out); }
.error-details-toggle { justify-self: start; padding: 0; border: 0; background: none; color: var(--muted-foreground); font-size: var(--text-xs); text-decoration: underline; cursor: pointer; }
.error-details-toggle:hover, .error-details-toggle:focus-visible { color: var(--foreground); }
.error-details { display: grid; gap: 2px; color: var(--muted-foreground); font-family: var(--font-mono); font-size: var(--text-xs); overflow-wrap: anywhere; }
.timeline-hook { display: grid; gap: var(--space-1); color: var(--muted-foreground); font-size: var(--text-sm); }
.timeline-hook-row { justify-self: start; display: flex; align-items: center; gap: var(--space-1); padding: 0; border: 0; background: none; color: var(--muted-foreground); font: inherit; cursor: pointer; }
.timeline-hook-row:hover, .timeline-hook-row:focus-visible { color: var(--foreground); }
.timeline-hook-row svg { transition: transform var(--duration-hover) ease; }
.timeline-hook-row svg.is-expanded { transform: rotate(180deg); }
.timeline-hook-body { display: grid; gap: 2px; padding-left: var(--space-4); font-family: var(--font-mono); font-size: var(--text-xs); overflow-wrap: anywhere; }

.jump-latest-enter-from { opacity: 0; transform: translateY(4px); }
@media (prefers-reduced-motion: reduce) {
  .jump-latest-enter-active { transition: none; }
}
</style>
