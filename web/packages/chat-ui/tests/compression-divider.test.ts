import { afterEach, describe, expect, it, vi } from 'vitest'
import { createApp, h, nextTick, type App } from 'vue'
import {
  TimelineCompressionDivider,
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
  summaryLoadFailed: 'Summary load failed',
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

const mounted: App[] = []

afterEach(() => {
  while (mounted.length > 0) mounted.pop()?.unmount()
  document.body.replaceChildren()
})

async function mountDivider(options: {
  info: ChatUiCompressionInfo
  loadSummary?: (input: { sessionId: string; segmentId: string }) => Promise<
    | { status: 'available'; summary: string }
    | { status: 'unavailable' }
  >
}) {
  const host = document.createElement('div')
  document.body.appendChild(host)
  const app = createApp({
    render: () => h(TimelineCompressionDivider, {
      info: options.info,
      labels,
      loadSummary: options.loadSummary,
    }, {
      'status-icon': ({ failed, showArchive }: { failed: boolean; showArchive: boolean }) =>
        failed ? h('i', { 'data-testid': 'warning-icon' }) : showArchive ? h('i', { 'data-testid': 'archive-icon' }) : null,
      'chevron-icon': ({ expanded }: { expanded: boolean }) => h('i', {
        'data-testid': 'chevron-icon',
        'data-expanded': String(expanded),
      }),
    }),
  })
  app.mount(host)
  mounted.push(app)
  return host
}

describe('TimelineCompressionDivider', () => {
  it('expands and loads the summary from the shared state machine', async () => {
    let resolveSummary!: (result: { status: 'available'; summary: string }) => void
    const loadSummary = vi.fn(() => new Promise<{ status: 'available'; summary: string }>(resolve => {
      resolveSummary = resolve
    }))
    const host = await mountDivider({ info: info(), loadSummary })

    const toggle = host.querySelector<HTMLElement>('[data-testid="compression-summary-toggle"]')
    expect(toggle).not.toBeNull()
    expect(host.querySelector('[data-testid="archive-icon"]')).not.toBeNull()
    expect(toggle?.getAttribute('aria-expanded')).toBe('false')

    toggle!.click()
    await nextTick()
    expect(toggle?.getAttribute('aria-expanded')).toBe('true')
    expect(loadSummary).toHaveBeenCalledWith({ sessionId: 'session-1', segmentId: 'segment-1' })
    expect(host.querySelector('[data-testid="compression-summary-panel"]')).not.toBeNull()
    expect(host.textContent).toContain('Loading summary')

    resolveSummary({ status: 'available', summary: 'summary text' })
    await vi.waitFor(() => {
      expect(host.querySelector('[data-testid="compression-summary-plaintext"]')?.textContent)
        .toBe('summary text')
    })

    toggle!.click()
    await nextTick()
    expect(toggle?.getAttribute('aria-expanded')).toBe('false')
    toggle!.click()
    await nextTick()
    expect(loadSummary).toHaveBeenCalledTimes(1)
  })

  it('renders failure details without requesting a summary', async () => {
    const loadSummary = vi.fn(async () => ({ status: 'available' as const, summary: 'x' }))
    const host = await mountDivider({
      info: info({ completed: false, status: 'failed', failureCode: 'E_FAIL', errorMessage: '  boom  ' }),
      loadSummary,
    })

    expect(host.querySelector('[data-testid="warning-icon"]')).not.toBeNull()
    host.querySelector<HTMLElement>('[data-testid="compression-summary-toggle"]')!.click()
    await nextTick()
    expect(host.querySelector('[data-testid="compression-failure-code"]')?.textContent?.trim()).toBe('E_FAIL')
    expect(host.querySelector('[data-testid="compression-failure-message"]')?.textContent).toBe('  boom  ')
    expect(loadSummary).not.toHaveBeenCalled()
  })

  it('renders unavailable summaries and keeps non-expandable rows static', async () => {
    const host = await mountDivider({
      info: info(),
      loadSummary: async () => ({ status: 'unavailable' as const }),
    })
    host.querySelector<HTMLElement>('[data-testid="compression-summary-toggle"]')!.click()
    await vi.waitFor(() => {
      expect(host.textContent).toContain('Summary unavailable')
    })

    const staticHost = await mountDivider({
      info: info({ completed: false, status: 'running' }),
    })
    expect(staticHost.querySelector('[data-testid="compression-summary-toggle"]')).toBeNull()
    expect(staticHost.querySelector('[data-testid="archive-icon"]')).toBeNull()
    expect(staticHost.textContent).toContain('Compressing')
  })
})
