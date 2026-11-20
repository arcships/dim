<script setup lang="ts">
import { computed } from "vue"
import { useI18n } from "vue-i18n"
import { useChatUiHost, type ChatUiTimelineToolCallBlock } from "@dimcode/chat-ui"
import { useWebTimeline } from "./primitives"
import { imageUrl } from "./rows"

/** The tool card's result panel for a result with images: its text, then each image the Session stores. */
const props = defineProps<{ block: ChatUiTimelineToolCallBlock }>()
const { t } = useI18n()
const context = useWebTimeline()
const sessionId = useChatUiHost().timeline?.scope?.sessionId
const result = computed(() => {
  const row = context.rowByKey(props.block.id)
  return row?.kind === "tool" ? row.result : undefined
})
const images = computed(() => {
  const id = sessionId?.value
  return id == null ? [] : (result.value?.images ?? []).map(image => ({ key: image.source.fileId, url: imageUrl(id, image.source.fileId) }))
})
</script>

<template>
  <div class="mt-1 space-y-2 rounded-md border border-border bg-card p-2" data-testid="web-tool-result">
    <pre v-if="result?.text" class="max-h-[240px] overflow-auto font-mono text-xs leading-5 text-foreground/80 whitespace-pre-wrap break-words">{{ result.text }}</pre>
    <img v-for="image in images" :key="image.key" :src="image.url" :alt="t('chatTimeline.toolImage')" loading="lazy" class="max-h-96 max-w-full rounded object-contain" data-testid="web-tool-image" />
  </div>
</template>
