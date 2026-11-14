<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from "vue"
import { useI18n } from "vue-i18n"
import { copyText } from "../../clipboard"
import { formatTimelineTimestamp } from "./timeline-time"

const props = defineProps<{ text: string; sessionId?: string; images?: import("../../rpc/protocol").ImageBlock[]; skills?: string[]; hooks?: number; at: number; quotable: boolean; branchable: boolean }>()
const emit = defineEmits<{ quote: [text: string]; branch: [text: string] }>()
const { t, locale } = useI18n()
const content = ref<HTMLElement>()
const overflows = ref(false)
const expanded = ref(false)
const copied = ref(false)
let copiedTimer: ReturnType<typeof setTimeout> | undefined

const sentTime = computed(() => formatTimelineTimestamp(new Date(props.at), locale.value))

onMounted(async () => {
  await nextTick()
  // Long messages clip at 300px until expanded.
  overflows.value = (content.value?.scrollHeight ?? 0) > 300
})
onBeforeUnmount(() => clearTimeout(copiedTimer))

async function copy(): Promise<void> {
  await copyText(props.text)
  copied.value = true
  clearTimeout(copiedTimer)
  copiedTimer = setTimeout(() => { copied.value = false }, 1500)
}
</script>

<template>
  <article class="user-message" data-role="user">
    <strong class="sr-only">{{ t("chat.you") }}</strong>
    <div v-if="sessionId && images?.length" class="flex flex-wrap gap-2">
      <img v-for="image in images" :key="image.source.fileId" :src="`/api/sessions/${encodeURIComponent(sessionId)}/files/${encodeURIComponent(image.source.fileId)}`" alt="Image attachment" loading="lazy" class="max-h-64 max-w-full rounded-lg" />
    </div>
    <div class="user-bubble">
      <div ref="content" class="user-bubble-content" :class="{ 'is-clipped': overflows && !expanded }">{{ text }}</div>
      <UButton v-if="overflows" class="user-bubble-expand" color="neutral" variant="ghost" size="xs" :aria-expanded="expanded" @click="expanded = !expanded">
        <UIcon name="i-lucide-chevron-down" class="size-3.5 user-expand-icon" :class="{ 'is-expanded': expanded }" aria-hidden="true" />
        <span>{{ expanded ? t("message.collapse") : t("message.expand") }}</span>
      </UButton>
    </div>
    <div v-if="skills?.length" class="user-skills">
      <div v-for="skill in skills" :key="skill" class="user-skill">{{ t("message.skillLoaded", { name: skill }) }}</div>
    </div>
    <div v-for="hook in hooks ?? 0" :key="hook" class="user-hook">{{ t("message.hookContext") }}</div>
    <div class="user-actions">
      <span class="user-sent-time timestamp">{{ sentTime }}</span>
      <UTooltip :text="t('message.copy')">
        <UButton color="neutral" variant="ghost" size="sm" square :icon="copied ? 'i-lucide-check' : 'i-lucide-copy'" :class="{ 'is-copied': copied }" :aria-label="t('message.copy')" @click="copy" />
      </UTooltip>
      <UTooltip v-if="branchable" :text="t('message.branch')">
        <UButton color="neutral" variant="ghost" size="sm" square icon="i-lucide-git-branch" :aria-label="t('message.branch')" @click="emit('branch', text)" />
      </UTooltip>
      <UTooltip v-if="quotable" :text="t('message.quote')">
        <UButton color="neutral" variant="ghost" size="sm" square icon="i-lucide-quote" :aria-label="t('message.quote')" @click="emit('quote', text)" />
      </UTooltip>
    </div>
  </article>
</template>

<style scoped>
/* The bubble column is capped at 85%; the shared bubble spec lives in global.css (.user-bubble). */
.user-message { display: flex; min-width: 0; flex-direction: column; align-items: flex-end; gap: var(--space-1); margin: 0; }
.user-bubble { min-width: 0; }
.user-bubble-content { max-width: 100%; white-space: pre-wrap; overflow-wrap: anywhere; }
.user-bubble-content.is-clipped { max-height: 300px; overflow: hidden; }
.user-bubble-expand { margin: var(--space-1) auto 0; }
.user-expand-icon { transition: transform var(--duration-press) var(--ease-out); }
.user-expand-icon.is-expanded { transform: rotate(180deg); }
.user-actions { display: flex; height: var(--control-sm); align-items: center; gap: var(--space-1); color: var(--muted-foreground); opacity: 0; transition: opacity var(--duration-hover) ease; }
.user-message:hover .user-actions, .user-message:focus-within .user-actions { opacity: 1; }
.user-skill, .user-hook { color: var(--muted-foreground); font-size: var(--text-xs); }
.user-sent-time { margin-right: var(--space-1); white-space: nowrap; }
.is-copied { color: var(--success); }
@media (hover: none) { .user-actions { opacity: 1; } }
</style>
