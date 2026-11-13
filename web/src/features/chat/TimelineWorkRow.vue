<script setup lang="ts">
import WorkStatusIcon from "./WorkStatusIcon.vue"
import { resolveToolDisplay } from "./tool-display"
import { computed } from "vue"
import { useI18n } from "vue-i18n"
import type { TranslationKey } from "../../i18n"
import type { ToolCategory, WorkItem } from "./work"
import TimelineWorkItemView from "./TimelineWorkItem.vue"

/**
 * A work group reads as one summary line ("Thought 2 times · 3 reads, 1 command")
 * that expands into a left-railed list; a single item shows on its own.
 */
const props = defineProps<{
  items: WorkItem[]
  expanded: boolean
  expandedItemIds: Record<string, boolean>
}>()
const emit = defineEmits<{ toggle: []; toggleItem: [itemId: string] }>()
const { t } = useI18n()

const CATEGORY_KEYS = {
  read: "work.read",
  edit: "work.edit",
  command: "work.command",
  search: "work.search",
  web_search: "work.webSearch",
  web_fetch: "work.webFetch",
  unknown: "work.unknown",
} as const satisfies Record<ToolCategory, TranslationKey>
const CATEGORY_ORDER = Object.keys(CATEGORY_KEYS) as ToolCategory[]

const summary = computed(() => {
  let text = ""
  let previous: "thinking" | ToolCategory | undefined
  const thinkingCount = props.items.filter((item) => item.kind === "thinking").length
  if (thinkingCount > 0) {
    text = t("work.thinkingSteps", { count: thinkingCount }, thinkingCount)
    previous = "thinking"
  }
  const counts = new Map<ToolCategory, number>()
  for (const item of props.items) {
    if (item.kind === "tool") counts.set(item.category, (counts.get(item.category) ?? 0) + 1)
  }
  for (const category of CATEGORY_ORDER) {
    const count = counts.get(category)
    if (count === undefined) continue
    if (previous !== undefined) text += t(previous === "thinking" ? "work.separator" : "work.actionSeparator")
    text += t(CATEGORY_KEYS[category], { count }, count)
    previous = category
  }
  return text
})
const activeItem = computed(() => props.items.find((item) => item.kind === "thinking" ? item.running : item.status === "running"))
const running = computed(() => activeItem.value !== undefined)
const status = computed(() => activeItem.value?.kind === "thinking" ? "thinking" : activeItem.value?.category ?? (props.items.some(item => item.kind === "tool" && item.status === "error") ? "error" : "done"))
const activeLabel = computed(() => {
  const item = activeItem.value
  if (!item) return summary.value
  if (item.kind === "thinking") return t("work.thinking")
  const display = resolveToolDisplay(item.name, item.input)
  return display.renderer === "generic" ? item.name : t(display.labelKey)
})
</script>

<template>
  <div class="timeline-work-row">
    <template v-if="items.length > 1">
      <button type="button" class="timeline-work-group-toggle" :class="{ 'is-running': running }" :aria-expanded="expanded" @click="emit('toggle')">
        <Transition name="work-icon" mode="out-in">
          <WorkStatusIcon :key="status" :state="status" :active="running" />
        </Transition>
        <span class="timeline-status-label">{{ activeLabel }}<span class="timeline-status-shine" aria-hidden="true">{{ activeLabel }}</span></span>
        <small v-if="activeItem?.kind === 'tool'" class="timeline-status-detail">{{ activeItem.primary }}</small>
        <UIcon name="i-lucide-chevron-right" class="timeline-status-chevron" :class="{ 'is-expanded': expanded }" aria-hidden="true" />
      </button>
      <div v-if="expanded" class="timeline-work-group-content">
        <TimelineWorkItemView
          v-for="item in items"
          :key="item.id"
          :item="item"
          :expanded="expandedItemIds[item.id] === true"
          @toggle="emit('toggleItem', item.id)"
        />
      </div>
    </template>
    <TimelineWorkItemView
      v-else-if="items[0]"
      :item="items[0]"
      :expanded="expandedItemIds[items[0].id] === true"
      @toggle="emit('toggleItem', items[0].id)"
    />
  </div>
</template>
