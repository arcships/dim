<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from "vue"
import { useI18n } from "vue-i18n"
import { Check, ChevronDown, Copy, GitBranch, Quote } from "lucide-vue-next"
import { copyText } from "../../clipboard"
import { formatTimelineTimestamp } from "./timeline-time"

const props = defineProps<{ text: string; skills?: string[]; at: number; quotable: boolean; branchable: boolean }>()
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
    <div class="user-bubble">
      <div ref="content" class="user-bubble-content" :class="{ 'is-clipped': overflows && !expanded }">{{ text }}</div>
      <button v-if="overflows" type="button" class="ui-button user-bubble-expand" data-variant="ghost" data-size="xs" :aria-expanded="expanded" @click="expanded = !expanded">
        <ChevronDown aria-hidden="true" :class="{ 'is-expanded': expanded }" />
        <span>{{ expanded ? t("message.collapse") : t("message.expand") }}</span>
      </button>
    </div>
    <div v-if="skills?.length" class="user-skills">
      <div v-for="skill in skills" :key="skill" class="user-skill">{{ t("message.skillLoaded", { name: skill }) }}</div>
    </div>
    <div class="user-actions">
      <span class="user-sent-time timestamp">{{ sentTime }}</span>
      <button class="ui-icon-button" data-variant="ghost" data-size="sm" type="button" v-tooltip="t('message.copy')" :aria-label="t('message.copy')" @click="copy">
        <Check v-if="copied" class="is-copied" aria-hidden="true" />
        <Copy v-else aria-hidden="true" />
      </button>
      <button v-if="branchable" class="ui-icon-button" data-variant="ghost" data-size="sm" type="button" v-tooltip="t('message.branch')" :aria-label="t('message.branch')" @click="emit('branch', text)">
        <GitBranch aria-hidden="true" />
      </button>
      <button v-if="quotable" class="ui-icon-button" data-variant="ghost" data-size="sm" type="button" v-tooltip="t('message.quote')" :aria-label="t('message.quote')" @click="emit('quote', text)">
        <Quote aria-hidden="true" />
      </button>
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
.user-bubble-expand svg { transition: transform var(--duration-press) var(--ease-out); }
.user-bubble-expand svg.is-expanded { transform: rotate(180deg); }
.user-actions { display: flex; height: var(--control-sm); align-items: center; gap: var(--space-1); color: var(--muted-foreground); opacity: 0; transition: opacity var(--duration-hover) ease; }
.user-message:hover .user-actions, .user-message:focus-within .user-actions { opacity: 1; }
.user-skill { color: var(--muted-foreground); font-size: var(--text-xs); }
.user-sent-time { margin-right: var(--space-1); white-space: nowrap; }
.is-copied { color: var(--success); }
@media (hover: none) { .user-actions { opacity: 1; } }
</style>
