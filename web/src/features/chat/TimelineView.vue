<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch, type ComponentPublicInstance } from "vue"
import { useI18n } from "vue-i18n"
import { useVirtualizer } from "@tanstack/vue-virtual"
import { ChevronDown, GitBranch, LoaderCircle } from "lucide-vue-next"
import type { TimelineRow } from "../../projection"
import ChatMarkdown from "./ChatMarkdown.vue"
import TimelineUserMessage from "./TimelineUserMessage.vue"
import TimelineWorkRow from "./TimelineWorkRow.vue"
import { groupRows } from "./work"
import { RAIL_MIN_MARKERS, timelineMarkers } from "../../timeline-markers"

/**
 * `quotable`: the composer is shown, so a message can be quoted into it.
 * `branchable`: the shown line is open, so a user message can open a child line.
 * `variant`: the main column or the narrower line panel.
 */
const props = defineProps<{
  rows: readonly TimelineRow[]
  running: boolean
  quotable: boolean
  branchable?: boolean
  empty?: string
  variant?: "main" | "panel"
}>()
const emit = defineEmits<{ quote: [text: string]; branch: [text: string, cut: number]; openLine: [branchId: string] }>()
const { t } = useI18n()
const viewport = ref<HTMLElement>()
const list = ref<HTMLElement>()
/** The view follows new output while the reader is within this distance of the end. */
const FOLLOW_THRESHOLD_PX = 96
const following = ref(true)
const expandedGroups = ref<Record<string, boolean>>({})
const expandedItems = ref<Record<string, boolean>>({})

const display = computed(() => groupRows(props.rows))

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
  return start < element.scrollTop - list.value.offsetTop
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
  if (element !== undefined) element.scrollTop = element.scrollHeight
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
              v-if="entry.kind === 'work'"
              :items="entry.items"
              :expanded="expandedGroups[entry.key] === true"
              :expanded-item-ids="expandedItems"
              @toggle="expandedGroups[entry.key] = !expandedGroups[entry.key]"
              @toggle-item="expandedItems[$event] = !expandedItems[$event]"
            />
            <TimelineUserMessage v-else-if="entry.row.kind === 'user'" :text="entry.row.text" :skills="entry.row.skills" :at="entry.row.at" :quotable="quotable" :branchable="branchable === true" @branch="onBranch(entry.row, $event)" @quote="emit('quote', $event)" />
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
              <GitBranch :size="14" aria-hidden="true" />
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
            <div v-else-if="entry.row.kind === 'error'" class="notice-row error-row" role="alert"><span>{{ entry.row.text }}</span></div>
            <p v-else class="timeline-notice">{{ t("timeline.noticeStopped") }}</p>
          </template>
        </div>
      </div>
      <div v-if="running" class="processing-row" role="status"><LoaderCircle :size="14" class="is-spinning" aria-hidden="true" /><span>{{ t("work.processing") }}</span></div>
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
      <button v-if="!following" class="ui-icon-button jump-latest" data-variant="secondary" data-size="md" type="button" :aria-label="t('timeline.scrollToBottom')" v-tooltip="t('timeline.scrollToBottom')" @click="jumpToLatest">
        <ChevronDown aria-hidden="true" />
      </button>
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

.jump-latest-enter-from { opacity: 0; transform: translateY(4px); }
@media (prefers-reduced-motion: reduce) {
  .jump-latest-enter-active { transition: none; }
}
</style>
