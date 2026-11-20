import { afterEach, describe, expect, it, vi } from 'vitest'
import { createApp, defineComponent, h, nextTick, ref, type App, type Component } from 'vue'
import {
  CHAT_UI_HOST,
  TimelineAssistantActions,
  chatUiEn,
  type ChatUiHost,
  type ChatUiAssistantActionCopyLabels,
  type ChatUiAssistantActionIcons,
  type ChatUiAssistantActionLabels,
  type ChatUiTimelineBlock,
} from '../src'

const labels: ChatUiAssistantActionLabels = {
  copyEntireMessage: 'Copy entire message',
  forkFromHere: 'Fork from here',
  retry: 'Retry',
}
const copyLabels: ChatUiAssistantActionCopyLabels = {
  runFailedPlaceholder: 'Run failed',
  userInterrupt: 'Interrupted',
}

const icon = (name: string): Component => defineComponent({
  name,
  setup: () => () => h('i', { 'data-testid': `icon-${name}` }),
})
const icons: ChatUiAssistantActionIcons = {
  check: icon('check'),
  copy: icon('copy'),
  fork: icon('fork'),
  retry: icon('retry'),
}

const Button = defineComponent({
  name: 'StubButton',
  inheritAttrs: false,
  props: { variant: String, size: String, disabled: Boolean },
  emits: ['click'],
  setup: (props, { emit, slots, attrs }) => () => h('button', {
    ...attrs,
    disabled: props.disabled,
    onClick: (event: MouseEvent) => emit('click', event),
  }, slots.default?.()),
})

const TooltipProvider = defineComponent({
  name: 'StubTooltipProvider',
  setup: (_, { slots }) => () => h('div', { 'data-testid': 'tooltip-provider' }, slots.default?.()),
})
const Tooltip = defineComponent({
  name: 'StubTooltip',
  setup: (_, { slots }) => () => h('div', { 'data-testid': 'tooltip' }, slots.default?.()),
})
const TooltipTrigger = defineComponent({
  name: 'StubTooltipTrigger',
  setup: (_, { slots }) => () => h('div', slots.default?.()),
})
const TooltipContent = defineComponent({
  name: 'StubTooltipContent',
  setup: (_, { slots }) => () => h('div', { 'data-testid': 'tooltip-content' }, slots.default?.()),
})

function block(overrides: Partial<ChatUiTimelineBlock> = {}): ChatUiTimelineBlock {
  return {
    id: 'block-1',
    kind: 'text',
    status: 'complete',
    content: 'hello',
    ...overrides,
  }
}

function copyHost(copy: ChatUiHost['commands']['copy']): ChatUiHost {
  return {
    session: ref(null),
    capabilities: ref({
      interactive: true,
      terminal: false,
      browserPanel: false,
      filePreview: false,
      mcpApps: false,
      notifications: false,
      attachments: false,
    }),
    commands: {
      send: vi.fn(async () => undefined),
      stop: vi.fn(async () => undefined),
      retry: vi.fn(async () => undefined),
      fork: vi.fn(async () => undefined),
      branch: vi.fn(async () => undefined),
      edit: vi.fn(async () => undefined),
      approve: vi.fn(async () => undefined),
      answer: vi.fn(async () => undefined),
      openFile: vi.fn(),
      copy,
    },
    i18n: { locale: ref('en'), t: key => chatUiEn[key] },
  }
}

function stubClipboard() {
  const writeText = vi.fn(async () => undefined)
  Object.defineProperty(navigator, 'clipboard', {
    configurable: true,
    value: { writeText },
  })
  return writeText
}

const mounted: App[] = []

afterEach(() => {
  while (mounted.length > 0) mounted.pop()?.unmount()
  document.body.replaceChildren()
  vi.restoreAllMocks()
})

async function mountActions(options: Partial<{
  blocks: ChatUiTimelineBlock[]
  isRunning: boolean
  forkMessageId: string | null
  canRetry: boolean
  retryDisabled: boolean
  actionsAlwaysVisible: boolean
  completedAt: Date | null
  retryLabel: string
  copySuccess: () => void
  chatUiHost: ChatUiHost
}> = {}) {
  const host = document.createElement('div')
  document.body.appendChild(host)
  const events = { fork: vi.fn(), retry: vi.fn() }
  const app = createApp({
    render: () => h(TimelineAssistantActions, {
      blocks: options.blocks ?? [block()],
      isRunning: options.isRunning ?? false,
      forkMessageId: options.forkMessageId === undefined ? 'message-1' : options.forkMessageId,
      canRetry: options.canRetry ?? true,
      forkDisabled: false,
      retryDisabled: options.retryDisabled ?? false,
      actionsAlwaysVisible: options.actionsAlwaysVisible ?? true,
      completionTimeOnly: false,
      retryLabel: options.retryLabel,
      completedAt: options.completedAt ?? null,
      labels,
      copyLabels,
      locale: 'en',
      icons,
      buttonComponent: Button,
      tooltipProviderComponent: TooltipProvider,
      tooltipComponent: Tooltip,
      tooltipTriggerComponent: TooltipTrigger,
      tooltipContentComponent: TooltipContent,
      onCopySuccess: options.copySuccess,
      onForkMessage: events.fork,
      onRetryMessage: events.retry,
    }),
  })
  const errors: unknown[] = []
  app.config.errorHandler = error => { errors.push(error) }
  if (options.chatUiHost !== undefined) app.provide(CHAT_UI_HOST, options.chatUiHost)
  app.mount(host)
  mounted.push(app)
  await nextTick()
  return { host, events, errors }
}

describe('TimelineAssistantActions', () => {
  it('copies the payload through navigator.clipboard without a host, shows the check icon, and notifies', async () => {
    const writeText = stubClipboard()
    const copySuccess = vi.fn()
    const { host } = await mountActions({ copySuccess })
    expect(host.querySelector('[data-testid="timeline-assistant-copy"]')).not.toBeNull()
    expect(host.querySelector('[data-testid="icon-copy"]')).not.toBeNull()

    host.querySelector<HTMLElement>('[data-testid="timeline-assistant-copy"]')!.click()
    await vi.waitFor(() => expect(writeText).toHaveBeenCalledWith('hello'))
    expect(copySuccess).toHaveBeenCalled()
    await nextTick()
    expect(host.querySelector('[data-testid="icon-check"]')).not.toBeNull()
  })

  it('copies the payload through the host', async () => {
    const writeText = stubClipboard()
    const copy = vi.fn(async () => undefined)
    const copySuccess = vi.fn()
    const { host } = await mountActions({ copySuccess, chatUiHost: copyHost(copy) })

    host.querySelector<HTMLElement>('[data-testid="timeline-assistant-copy"]')!.click()
    await vi.waitFor(() => expect(copySuccess).toHaveBeenCalledOnce())
    expect(copy).toHaveBeenCalledWith('hello')
    expect(writeText).not.toHaveBeenCalled()
    await nextTick()
    expect(host.querySelector('[data-testid="icon-check"]')).not.toBeNull()
  })

  it('keeps the copy icon when the host copy rejects', async () => {
    const copy = vi.fn(async () => { throw new Error('denied') })
    const copySuccess = vi.fn()
    const { host, errors } = await mountActions({ copySuccess, chatUiHost: copyHost(copy) })

    host.querySelector<HTMLElement>('[data-testid="timeline-assistant-copy"]')!.click()
    await vi.waitFor(() => expect(errors).toHaveLength(1))
    expect(copy).toHaveBeenCalledWith('hello')
    expect(copySuccess).not.toHaveBeenCalled()
    expect(host.querySelector('[data-testid="icon-copy"]')).not.toBeNull()
  })

  it('shows fork and retry actions with the custom retry label', async () => {
    const { host, events } = await mountActions({ retryLabel: 'Try again' })
    expect(host.textContent).toContain('Try again')
    host.querySelector<HTMLElement>('[data-testid="timeline-assistant-fork"]')!.click()
    expect(events.fork).toHaveBeenCalled()
    host.querySelector<HTMLElement>('[data-testid="timeline-assistant-retry"]')!.click()
    expect(events.retry).toHaveBeenCalled()
  })

  it('hides actions while a block is running', async () => {
    const { host } = await mountActions({
      blocks: [block({ status: 'running' })],
      canRetry: true,
    })
    expect(host.querySelector('[data-testid="timeline-assistant-actions"]')).toBeNull()
  })

  it('renders the completion time and omits fork without an anchor', async () => {
    const { host } = await mountActions({
      forkMessageId: null,
      canRetry: false,
      blocks: [],
      actionsAlwaysVisible: true,
      completedAt: new Date('2026-08-29T12:34:00.000Z'),
    })
    expect(host.querySelector('[data-testid="timeline-assistant-fork"]')).toBeNull()
    expect(host.querySelector('[data-testid="timeline-assistant-completed-time"]')).not.toBeNull()
    expect(host.querySelector('[data-testid="timeline-assistant-actions"]')).not.toBeNull()
  })

  it('does not render buttons for an empty final message', async () => {
    const { host } = await mountActions({ blocks: [], canRetry: false, forkMessageId: null })
    expect(host.querySelector('[data-testid="timeline-assistant-actions"]')).toBeNull()
  })
})
