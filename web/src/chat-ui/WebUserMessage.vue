<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from "vue"
import { useI18n } from "vue-i18n"
import {
  TimelineUserActions,
  TimelineUserBubble,
  TimelineUserContent,
  getLinkIconKind,
  useChatUiHost,
  type ChatUiTimelineRowMessage,
} from "@dimcode/chat-ui"
import { copyText } from "../clipboard"
import { skillSegments } from "../skill-token"
import { formatTimelineTimestamp } from "../features/chat/timeline-time"
import { useToastStore } from "../stores/toast"
import { Button, Passthrough, fileIconUrl, icon, icons, tooltip, useWebTimeline } from "./primitives"

const props = withDefaults(defineProps<{ message: ChatUiTimelineRowMessage; readOnly?: boolean; canBranch?: boolean; branchDisabled?: boolean }>(), {
  readOnly: true,
  canBranch: false,
  branchDisabled: false,
})
const emit = defineEmits<{ "branch-message": []; "fork-message": [] }>()
const { t, locale } = useI18n()
const toast = useToastStore()
const context = useWebTimeline()
const actions = useChatUiHost().timeline?.actions
const pending = computed(() => actions?.pending.value === true)
const canFork = computed(() => !props.readOnly && actions?.canFork(props.message.id) === true)
/** This message's turn is still running, so no cut before it is a rest point yet. */
const inRunningTurn = computed(() => props.branchDisabled || context.runningUserMessageIds.value.has(props.message.id))
/** Why branching is off for this message, or undefined when it is on. */
const branchBlocked = computed(() => {
  const branch = context.actions.value.branch
  if (!branch.available) return branch.reason
  return inRunningTurn.value ? t("chatTimeline.unavailable.branchRunning") : undefined
})
const userLabels = computed(() => {
  const labels = context.labels.value.user
  return {
    ...labels,
    ...(branchBlocked.value === undefined ? {} : { branch: branchBlocked.value }),
    ...(inRunningTurn.value ? { fork: t("chatTimeline.unavailable.forkRunning") } : {}),
  }
})
const row = computed(() => {
  const source = context.rowByKey(props.message.id)
  return source?.kind === "user" ? source : undefined
})
/** The message as it is drawn: each `/skill:<name>` token one chip, the words between it through the shared renderer. */
const parts = computed(() => skillSegments(props.message.content))
const timeText = computed(() => formatTimelineTimestamp(props.message.createdAt, locale.value))
const copied = ref(false)
let copiedTimer: ReturnType<typeof setTimeout> | undefined
onBeforeUnmount(() => clearTimeout(copiedTimer))

const contentIcons = { skill: icon("i-lucide-sparkles"), commit: icon("i-lucide-git-commit-horizontal"), genericLink: icon("i-lucide-link") }
const actionIcons = {
  withdraw: icon("i-lucide-undo-2"),
  copy: icons.copy,
  check: icons.check,
  edit: icon("i-lucide-pencil"),
  fork: icons.gitFork,
  branch: icons.gitBranch,
}
const quoteIcon = icon("i-lucide-quote")

async function copy(): Promise<void> {
  try {
    await copyText(props.message.content)
  } catch {
    toast.push({ tone: "danger", title: t("chat.copyFailed") })
    return
  }
  copied.value = true
  clearTimeout(copiedTimer)
  copiedTimer = setTimeout(() => { copied.value = false }, 1500)
}
</script>

<template>
  <article class="web-user-message" data-role="user">
    <strong class="sr-only">{{ t("chat.you") }}</strong>
    <div v-if="message.attachments?.length" class="flex flex-wrap justify-end gap-2">
      <img v-for="attachment in message.attachments" :key="attachment.id" :src="attachment.url" :alt="t('chatTimeline.imageAttachment')" loading="lazy" class="max-h-64 max-w-full rounded-lg" />
    </div>
    <TimelineUserBubble :message-id="message.id" class="web-user-bubble">
      <template v-for="(part, index) in parts" :key="index">
        <span v-if="part.skill" class="skill-chip">{{ part.text }}</span>
        <TimelineUserContent
          v-else
          :content="part.text"
          :icons="contentIcons"
          :resolve-file-icon-lookup-path="(path: string) => path"
          :get-file-icon-url="fileIconUrl"
          :normalize-attachment-reference="(value: string) => value"
          :is-pasted-text-name="() => false"
          :get-link-icon-kind="getLinkIconKind"
        />
      </template>
    </TimelineUserBubble>
    <div v-for="skill in row?.skills ?? []" :key="skill" class="web-user-note">{{ t("message.skillLoaded", { name: skill }) }}</div>
    <div v-for="hook in row?.hooks ?? 0" :key="hook" class="web-user-note">{{ t("message.hookContext") }}</div>
    <div class="flex items-center gap-1">
      <TimelineUserActions
        :time-text="timeText"
        :can-withdraw="false"
        :can-edit="false"
        :can-fork="canFork"
        :can-branch="canBranch"
        :branch-disabled="branchBlocked !== undefined"
        :action-pending="pending || inRunningTurn"
        :copied="copied"
        :labels="userLabels"
        :icons="actionIcons"
        :button-component="Button"
        :tooltip-provider-component="Passthrough"
        :tooltip-component="tooltip.tooltipComponent"
        :tooltip-trigger-component="tooltip.tooltipTriggerComponent"
        :tooltip-content-component="tooltip.tooltipContentComponent"
        @copy="copy"
        @fork="emit('fork-message')"
        @branch="emit('branch-message')"
      />
      <div v-if="context.quotable.value || !readOnly" class="web-user-extra-actions flex h-7 items-center gap-1 text-muted-foreground opacity-0 transition-opacity duration-150 group-hover:opacity-100 group-focus-within:opacity-100">
        <component :is="tooltip.tooltipComponent" v-if="!readOnly">
          <component :is="tooltip.tooltipTriggerComponent">
            <Button data-testid="web-user-edit" class="h-7 w-7 justify-center rounded-lg" disabled>
              <component :is="actionIcons.edit" class="h-3.5 w-3.5" />
            </Button>
          </component>
          <component :is="tooltip.tooltipContentComponent">{{ t("chatTimeline.unavailable.edit") }}</component>
        </component>
        <component :is="tooltip.tooltipComponent" v-if="context.quotable.value">
          <component :is="tooltip.tooltipTriggerComponent">
            <Button data-testid="web-user-quote" class="h-7 w-7 justify-center rounded-lg" @click="context.quote(message.content)">
              <component :is="quoteIcon" class="h-3.5 w-3.5" />
            </Button>
          </component>
          <component :is="tooltip.tooltipContentComponent">{{ t("message.quote") }}</component>
        </component>
      </div>
    </div>
  </article>
</template>

<style scoped>
.web-user-message { display: flex; min-width: 0; flex-direction: column; align-items: flex-end; gap: var(--space-1); margin-bottom: var(--space-2); }
.web-user-bubble { max-width: 85%; white-space: pre-wrap; overflow-wrap: anywhere; }
.skill-chip { padding: 0 0.25em; border-radius: var(--radius-sm); background: var(--skill-chip); font-weight: var(--weight-ui-medium, 600); }
.web-user-note { color: var(--muted-foreground); font-size: var(--text-xs); }
</style>
