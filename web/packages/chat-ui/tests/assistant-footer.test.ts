import { describe, expect, it } from 'vitest'
import { nextTick, ref } from 'vue'
import {
  useTimelineAssistantFooter,
  type ChatUiTimelineBlock,
  type ChatUiTimelineCopyLabels,
} from '../src'

const labels: ChatUiTimelineCopyLabels = {
  runFailedPlaceholder: 'Run failed',
  userInterrupt: 'Interrupted',
}

function block(overrides: Partial<ChatUiTimelineBlock> = {}): ChatUiTimelineBlock {
  return {
    id: 'block-1',
    kind: 'text',
    status: 'complete',
    content: 'hello',
    ...overrides,
  }
}

function createOptions(overrides: Partial<Parameters<typeof useTimelineAssistantFooter>[0]> = {}) {
  return {
    blocks: [block()],
    isRunning: false,
    canRetry: false,
    forkMessageId: null,
    isPopoutWindow: false,
    actionsAlwaysVisible: false,
    completedAt: null,
    labels,
    locale: 'en',
    ...overrides,
  }
}

describe('useTimelineAssistantFooter', () => {
  it('hides every action while the message or any block is running', async () => {
    const isRunning = ref(false)
    const blocks = ref([block({ status: 'running' })])
    const state = useTimelineAssistantFooter(createOptions({ blocks, isRunning, canRetry: true }))
    expect(state.hasRunningBlock.value).toBe(true)
    expect(state.isMessageFinal.value).toBe(false)
    expect(state.hasCopyPayload.value).toBe(false)
    expect(state.hasActions.value).toBe(false)

    blocks.value = [block()]
    isRunning.value = true
    await nextTick()
    expect(state.isMessageFinal.value).toBe(false)
    expect(state.hasActions.value).toBe(false)
  })

  it('gates retry, fork, and always-visible completion time', async () => {
    const canRetry = ref(false)
    const forkMessageId = ref<string | null>(null)
    const completedAt = ref<Date | null>(null)
    const actionsAlwaysVisible = ref(false)
    const state = useTimelineAssistantFooter(createOptions({
      blocks: [],
      canRetry,
      forkMessageId,
      completedAt,
      actionsAlwaysVisible,
    }))
    expect(state.hasActions.value).toBe(false)

    canRetry.value = true
    await nextTick()
    expect(state.hasActions.value).toBe(true)

    canRetry.value = false
    forkMessageId.value = 'message-1'
    await nextTick()
    expect(state.hasActions.value).toBe(true)

    forkMessageId.value = null
    completedAt.value = new Date('2026-08-29T12:34:00.000Z')
    await nextTick()
    expect(state.hasActions.value).toBe(false)

    actionsAlwaysVisible.value = true
    await nextTick()
    expect(state.hasActions.value).toBe(true)
  })

  it('hides fork in popout windows and formats completion time', () => {
    const isPopoutWindow = ref(true)
    const completedAt = ref<Date | null>(new Date('2026-08-29T12:34:00.000Z'))
    const state = useTimelineAssistantFooter(createOptions({
      blocks: [],
      forkMessageId: 'message-1',
      isPopoutWindow,
      completedAt,
      completionTimeOnly: true,
    }))
    expect(state.hasActions.value).toBe(false)
    expect(state.completedTimeText.value).not.toBe('')

    isPopoutWindow.value = false
    expect(state.hasActions.value).toBe(true)
    expect(state.completedTimeText.value).not.toBe('')
  })

  it('uses the run-failed placeholder in the copy payload', () => {
    const state = useTimelineAssistantFooter(createOptions({
      blocks: [block({ kind: 'status', statusType: 'error', content: '' })],
    }))
    expect(state.copyPayload.value).toBe('Run failed')
  })
})
