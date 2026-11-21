import { describe, expect, it, vi } from 'vitest'
import { nextTick, ref } from 'vue'
import {
  useTimelineCompressionSummary,
  type ChatUiCompressionInfo,
  type ChatUiCompressionLabels,
} from '../src'

const labels: ChatUiCompressionLabels = {
  compressing: 'Compressing',
  failed: 'Compression failed',
  skipped: 'Skipped',
  cancelled: 'Cancelled',
  partial: 'partial',
  autoCompacted: 'Auto compacted',
  compressed: 'Compressed',
  summaryLoading: 'Loading summary',
  summaryUnavailable: 'Summary unavailable',
  summaryLoadFailed: 'Summary failed',
  retry: 'Retry',
}

function info(overrides: Partial<ChatUiCompressionInfo> = {}): ChatUiCompressionInfo {
  return {
    id: 'row-1',
    status: 'completed',
    completed: true,
    partial: false,
    auto: false,
    sessionId: 'session-1',
    segmentId: 'segment-1',
    ...overrides,
  }
}

describe('useTimelineCompressionSummary', () => {
  it('formats the divider text for each status', async () => {
    const value = ref<ChatUiCompressionInfo | null>(info())
    const state = useTimelineCompressionSummary({ info: value, labels })
    expect(state.compressionText.value).toBe('Compressed')
    expect(state.summaryPanelId.value).toBe('compression-summary-row-1')

    value.value = info({ status: 'running' })
    await nextTick()
    expect(state.compressionText.value).toBe('Compressing')
    value.value = info({ status: 'failed' })
    await nextTick()
    expect(state.compressionText.value).toBe('Compression failed')
    value.value = info({ status: 'skipped' })
    await nextTick()
    expect(state.compressionText.value).toBe('Skipped')
    value.value = info({ status: 'cancelled' })
    await nextTick()
    expect(state.compressionText.value).toBe('Cancelled')
    value.value = info({ partial: true, auto: true })
    await nextTick()
    expect(state.compressionText.value).toBe('Auto compacted (partial)')
  })

  it('expands only completed segments or failure details', async () => {
    const value = ref<ChatUiCompressionInfo | null>(info())
    const state = useTimelineCompressionSummary({ info: value, labels })
    expect(state.canExpand.value).toBe(true)

    value.value = info({ completed: false })
    await nextTick()
    expect(state.canExpand.value).toBe(false)

    value.value = info({ completed: false, status: 'failed', errorMessage: '  boom  ', failureCode: 'E_FAIL' })
    await nextTick()
    expect(state.canExpand.value).toBe(true)
    expect(state.hasFailureDetails.value).toBe(true)
    expect(state.failureErrorMessage.value).toBe('  boom  ')
  })

  it('marks unavailable and failed loads without throwing', async () => {
    const unavailable = useTimelineCompressionSummary({
      info: info(),
      labels,
      loadSummary: async () => ({ status: 'unavailable' as const }),
    })
    await unavailable.loadSummary()
    expect(unavailable.summaryStatus.value).toBe('unavailable')

    const failed = useTimelineCompressionSummary({
      info: info(),
      labels,
      loadSummary: async () => {
        throw new Error('network')
      },
    })
    await failed.loadSummary()
    expect(failed.summaryStatus.value).toBe('failed')
  })

  it('does not load failure details and ignores missing segment ids', async () => {
    const loadSummary = vi.fn(async () => ({ status: 'available' as const, summary: 'x' }))
    const failed = useTimelineCompressionSummary({
      info: info({ status: 'failed', errorMessage: 'boom' }),
      labels,
      loadSummary,
    })
    failed.toggleSummary()
    expect(failed.expanded.value).toBe(true)
    expect(loadSummary).not.toHaveBeenCalled()

    const missingSegment = useTimelineCompressionSummary({
      info: info({ segmentId: undefined }),
      labels,
      loadSummary,
    })
    await missingSegment.loadSummary()
    expect(loadSummary).not.toHaveBeenCalled()
    expect(missingSegment.summaryStatus.value).toBe('idle')
  })
})
