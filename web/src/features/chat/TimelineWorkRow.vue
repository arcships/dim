<script setup lang="ts">
import { computed } from "vue"
import { ChevronRight } from "lucide-vue-next"
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
const running = computed(() => props.items.some((item) => item.kind === "thinking" ? item.running : item.status === "running"))
</script>

<template>
  <div class="timeline-work-row">
    <template v-if="items.length > 1">
      <button type="button" class="timeline-work-group-toggle" :class="{ 'is-running': running }" :aria-expanded="expanded" @click="emit('toggle')">
        <ChevronRight :size="14" aria-hidden="true" :class="{ 'is-expanded': expanded }" />
        <span>{{ summary }}</span>
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
