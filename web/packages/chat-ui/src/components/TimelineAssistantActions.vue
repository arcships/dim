<script setup lang="ts">
import { computed, onBeforeUnmount, ref, type Component } from 'vue'
import type { ChatUiTimelineBlock } from '../model/timeline'
import { useTimelineAssistantFooter } from '../timeline/assistant-footer'
import { useChatUiCopy } from './clipboard'
import type { ChatUiAssistantActionCopyLabels, ChatUiAssistantActionIcons, ChatUiAssistantActionLabels } from './assistant-actions'

defineOptions({
  name: 'TimelineAssistantActions',
})

const props = defineProps<{
  blocks: readonly ChatUiTimelineBlock[]
  isRunning: boolean
  forkMessageId: string | null
  canRetry: boolean
  forkDisabled?: boolean
  retryDisabled: boolean
  actionsAlwaysVisible?: boolean
  completionTimeOnly?: boolean
  retryLabel?: string
  completedAt: Date | null
  labels: ChatUiAssistantActionLabels
  copyLabels: ChatUiAssistantActionCopyLabels
  locale: string
  icons: ChatUiAssistantActionIcons
  buttonComponent?: Component
  tooltipProviderComponent: Component
  tooltipComponent: Component
  tooltipTriggerComponent: Component
  tooltipContentComponent: Component
  onCopySuccess?: () => void
}>()

const emit = defineEmits<{
  'fork-message': []
  'retry-message': []
}>()

const buttonComponent = computed<Component | string>(() => props.buttonComponent ?? 'button')
const copy = useChatUiCopy()
const copied = ref(false)
let copiedTimer: ReturnType<typeof setTimeout> | undefined

const {
  hasCopyPayload,
  copyPayload,
  hasActions,
  completedTimeText,
} = useTimelineAssistantFooter({
  blocks: () => props.blocks,
  isRunning: () => props.isRunning,
  canRetry: () => props.canRetry,
  forkMessageId: () => props.forkMessageId,
  isPopoutWindow: false,
  actionsAlwaysVisible: () => props.actionsAlwaysVisible === true,
  completedAt: () => props.completedAt,
  labels: () => props.copyLabels,
  locale: () => props.locale,
  completionTimeOnly: () => props.completionTimeOnly === true,
})

async function copyText(content: string): Promise<void> {
  await copy(content)
  copied.value = true
  props.onCopySuccess?.()
  if (copiedTimer !== undefined) clearTimeout(copiedTimer)
  copiedTimer = setTimeout(() => {
    copied.value = false
    copiedTimer = undefined
  }, 1500)
}

onBeforeUnmount(() => {
  if (copiedTimer !== undefined) clearTimeout(copiedTimer)
})
</script>

<template>
  <component :is="tooltipProviderComponent" v-if="hasActions">
    <div
      class="mt-2 flex h-7 items-center gap-2 transition-opacity duration-150"
      :class="actionsAlwaysVisible ? 'opacity-100' : 'opacity-0 group-hover:opacity-100'"
      data-timeline-assistant-actions-slot="true"
      data-testid="timeline-assistant-actions"
    >
      <component :is="tooltipComponent" v-if="hasCopyPayload">
        <component :is="tooltipTriggerComponent" as-child>
          <component
            :is="buttonComponent"
            variant="ghost"
            size="icon-sm"
            data-testid="timeline-assistant-copy"
            class="inline-flex size-7 cursor-pointer items-center justify-center rounded-lg border-none bg-transparent p-0 text-muted-foreground shadow-none transition-colors hover:bg-accent hover:text-foreground"
            @click="copyText(copyPayload)"
          >
            <component :is="copied ? icons.check : icons.copy" class="h-3.5 w-3.5" :class="{ 'text-green-600': copied }" />
          </component>
        </component>
        <component :is="tooltipContentComponent" side="top">{{ labels.copyEntireMessage }}</component>
      </component>
      <component :is="tooltipComponent" v-if="forkMessageId">
        <component :is="tooltipTriggerComponent" as-child>
          <component
            :is="buttonComponent"
            variant="ghost"
            size="icon-sm"
            data-testid="timeline-assistant-fork"
            :disabled="forkDisabled"
            class="inline-flex size-7 cursor-pointer items-center justify-center rounded-lg border-none bg-transparent p-0 text-muted-foreground shadow-none transition-colors hover:bg-accent hover:text-foreground"
            @click="!forkDisabled && emit('fork-message')"
          >
            <component :is="icons.fork" class="h-3.5 w-3.5" />
          </component>
        </component>
        <component :is="tooltipContentComponent" side="top">{{ labels.forkFromHere }}</component>
      </component>
      <component :is="tooltipComponent" v-if="canRetry">
        <component :is="tooltipTriggerComponent" as-child>
          <component
            :is="buttonComponent"
            variant="ghost"
            size="icon-sm"
            data-testid="timeline-assistant-retry"
            :disabled="retryDisabled"
            class="inline-flex size-7 cursor-pointer items-center justify-center rounded-lg border-none bg-transparent p-0 text-muted-foreground shadow-none transition-colors hover:bg-accent hover:text-foreground"
            @click="emit('retry-message')"
          >
            <component :is="icons.retry" class="h-3.5 w-3.5" />
          </component>
        </component>
        <component :is="tooltipContentComponent" side="top">{{ retryLabel ?? labels.retry }}</component>
      </component>
      <span
        v-if="completedTimeText"
        data-testid="timeline-assistant-completed-time"
        class="shrink-0 whitespace-nowrap text-sm tabular-nums leading-none text-muted-foreground"
      >
        {{ completedTimeText }}
      </span>
    </div>
  </component>
</template>
