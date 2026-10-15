<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch, type ComponentPublicInstance } from "vue"
import { useI18n } from "vue-i18n"
import { useVirtualizer } from "@tanstack/vue-virtual"
import { ChevronDown, LoaderCircle } from "lucide-vue-next"
import type { TimelineRow } from "../../projection"
import ChatMarkdown from "./ChatMarkdown.vue"
import TimelineUserMessage from "./TimelineUserMessage.vue"
import TimelineWorkRow from "./TimelineWorkRow.vue"
import { groupRows } from "./work"

const props = defineProps<{ rows: readonly TimelineRow[]; running: boolean }>()
const { t } = useI18n()
const viewport = ref<HTMLElement>()
/** The view follows new output while the reader is within this distance of the end. */
const FOLLOW_THRESHOLD_PX = 96
const following = ref(true)
const expandedGroups = ref<Record<string, boolean>>({})
const expandedItems = ref<Record<string, boolean>>({})

const display = computed(() => groupRows(props.rows))

const virtualizer = useVirtualizer(computed(() => ({
  count: display.value.length,
  getScrollElement: () => viewport.value ?? null,
  estimateSize: () => 72,
  overscan: 6,
  getItemKey: (index: number) => display.value[index]!.key,
})))
const virtualRows = computed(() => virtualizer.value.getVirtualItems())
const totalSize = computed(() => virtualizer.value.getTotalSize())

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
  <div class="timeline-shell">
    <div ref="viewport" class="timeline app-scroll-viewport" @scroll.passive="onScroll">
      <p v-if="display.length === 0 && !running" class="timeline-empty">{{ t("chat.emptyTimeline") }}</p>
      <div class="virtual-timeline" :style="{ height: `${totalSize}px`, position: 'relative' }">
        <div
          v-for="item in virtualRows"
          :key="String(item.key)"
          :ref="measure"
          class="virtual-timeline-row"
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
            <TimelineUserMessage v-else-if="entry.row.kind === 'user'" :text="entry.row.text" :at="entry.row.at" />
            <article v-else-if="entry.row.kind === 'assistant'" class="timeline-row" data-role="assistant">
              <strong class="sr-only">{{ t("chat.assistant") }}</strong>
              <ChatMarkdown :content="entry.row.text" :final="!entry.row.streaming" :message-id="entry.row.key" />
            </article>
            <div v-else-if="entry.row.kind === 'error'" class="notice-row error-row" role="alert"><span>{{ entry.row.text }}</span></div>
            <p v-else class="timeline-notice">{{ t("timeline.noticeStopped") }}</p>
          </template>
        </div>
      </div>
      <div v-if="running" class="processing-row" role="status"><LoaderCircle :size="14" class="is-spinning" aria-hidden="true" /><span>{{ t("work.processing") }}</span></div>
    </div>
    <Transition name="jump-latest">
      <button v-if="!following" class="ui-icon-button jump-latest" data-variant="secondary" data-size="md" type="button" :aria-label="t('timeline.scrollToBottom')" v-tooltip="t('timeline.scrollToBottom')" @click="jumpToLatest">
        <ChevronDown aria-hidden="true" />
      </button>
    </Transition>
  </div>
</template>

<style scoped>
.jump-latest-enter-active { transition: opacity var(--duration-popover) var(--ease-out), transform var(--duration-popover) var(--ease-out); }
.jump-latest-enter-from { opacity: 0; transform: translateY(4px); }
@media (prefers-reduced-motion: reduce) {
  .jump-latest-enter-active { transition: none; }
}
</style>
