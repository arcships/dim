<script setup lang="ts">
import { computed, ref } from "vue"
import { useI18n } from "vue-i18n"
import type { TimelineRow } from "../projection"
import { useWebTimeline } from "./primitives"

/**
 * The shared row's `turnOutput` slot. A line opens its transcript and a hook
 * follow-up unfolds its body, the two notice rows that act; the others are
 * text the host's `systemNoticeText` hook gives.
 */
const props = defineProps<{ messageIds: readonly string[] }>()
const { t } = useI18n()
const context = useWebTimeline()
const expanded = ref(false)

type Acting = Extract<TimelineRow, { kind: "line" | "hook-follow-up" }>
const rows = computed(() => props.messageIds
  .map(id => context.rowByKey(id))
  .filter((row): row is Acting => row?.kind === "line" || row?.kind === "hook-follow-up"))

function shortId(id: string): string {
  return id.slice(0, 8)
}
</script>

<template>
  <template v-for="row in rows" :key="row.key">
    <button
      v-if="row.kind === 'line'"
      type="button"
      class="timeline-line-marker"
      :disabled="row.failed === true"
      :aria-label="t('lines.openLine', { name: row.purpose || shortId(row.branchId) })"
      @click="row.failed !== true && context.openLine(row.branchId)"
    >
      <UIcon name="i-lucide-git-branch" class="size-3.5 shrink-0" aria-hidden="true" />
      <span class="timeline-line-name">{{ row.purpose || shortId(row.branchId) }}</span>
      <code>{{ shortId(row.branchId) }}</code>
      <span v-if="row.failed" class="timeline-line-failed">{{ t("lines.state.creationFailed") }}</span>
      <span v-else-if="row.before" class="timeline-line-cut">{{ t("timeline.beforeMessage", { message: row.before }) }}</span>
    </button>
    <div v-else class="web-hook">
      <button type="button" class="web-hook-row" :aria-expanded="expanded" @click="expanded = !expanded">
        <span>{{ t("timeline.hookFollowUp", { count: row.lines }, row.lines) }}</span>
        <UIcon name="i-lucide-chevron-down" class="size-3.5" :class="{ 'is-expanded': expanded }" aria-hidden="true" />
      </button>
      <div v-if="expanded" class="web-hook-body">
        <div v-for="(line, index) in row.body.split('\n')" :key="index">{{ line }}</div>
      </div>
    </div>
  </template>
</template>

<style scoped>
.web-hook { display: grid; gap: var(--space-1); margin-bottom: var(--space-2); color: var(--muted-foreground); font-size: var(--text-sm); }
.web-hook-row { justify-self: start; display: flex; align-items: center; gap: var(--space-1); padding: 0; border: 0; background: none; color: var(--muted-foreground); font: inherit; cursor: pointer; }
.web-hook-row:hover, .web-hook-row:focus-visible { color: var(--foreground); }
.web-hook-row svg { transition: transform var(--duration-hover) ease; }
.web-hook-row svg.is-expanded { transform: rotate(180deg); }
.web-hook-body { display: grid; gap: 2px; padding-left: var(--space-4); font-family: var(--font-mono); font-size: var(--text-xs); overflow-wrap: anywhere; }
</style>
