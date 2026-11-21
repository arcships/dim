import { afterEach, describe, expect, it, vi } from 'vitest'
import { createApp, defineComponent, h, nextTick, ref, type App, type Component } from 'vue'
import {
  CHAT_UI_HOST,
  TimelineAssistantMessage,
  chatUiEn,
  type ChatUiHost,
  type ChatUiAssistantMessageBlock,
  type ChatUiAssistantMessageIcons,
  type ChatUiAssistantMessageLabels,
  type ChatUiTurnTerminalState,
  type ChatUiTimelineToolSummaryCategory,
} from '../src'

const labels: ChatUiAssistantMessageLabels = {
  runFailedPlaceholder: 'Run failed',
  userInterrupt: 'Stopped',
  backgroundTaskCommand: 'command',
  backgroundTaskStatus: {
    completed: 'completed',
    failed: 'failed',
    cancelled: 'cancelled',
  },
  open: 'Open',
  copyMessage: 'Copy message',
  copyEntireMessage: 'Copy entire message',
  terminalErrorDetailsTitle: 'Error details',
  copy: 'Copy',
  planRenew: 'Renew plan',
  workAction: {
    read: 'Read',
    edit: 'Edit',
    command: 'Command',
    search: 'Search',
    webSearch: 'Web search',
    webFetch: 'Web fetch',
    subagent: 'Subagent',
    browser: 'Browser',
    computerUse: 'Computer use',
    wait: 'Wait',
    skill: 'Skill',
    unknown: 'Tool',
  },
  workActionSeparator: ', ',
  workActionMoreSuffix: ' + more',
  workSummary: input => `summary:${input.terminalStatus ?? 'none'}:${input.actionsText}:${input.durationText ?? 'x'}`,
}

const icon = (name: string): Component => defineComponent({
  name,
  setup: () => () => h('i', { 'data-testid': `icon-${name}` }),
})

const icons: ChatUiAssistantMessageIcons = {
  chevron: icon('chevron'),
  copy: icon('copy'),
  check: icon('check'),
  externalLink: icon('external-link'),
  listChecks: icon('list-checks'),
  bot: icon('bot'),
  terminal: icon('terminal'),
  layers: icon('layers'),
  triangleAlert: icon('triangle-alert'),
  octagonPause: icon('octagon-pause'),
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

const passthrough = (name: string): Component => defineComponent({
  name,
  setup: (_, { slots }) => () => h('div', slots.default?.()),
})

const emptyComponent = (name: string): Component => defineComponent({
  name,
  setup: () => () => null,
})

const testIdComponent = (name: string): Component => defineComponent({
  name,
  inheritAttrs: false,
  setup: (_, { attrs }) => () => h('div', { ...attrs, 'data-testid': name }),
})

let capturedGroupBlocks: readonly ChatUiAssistantMessageBlock[] = []
const BlockGroupStub = defineComponent({
  name: 'StubBlockGroup',
  inheritAttrs: false,
  props: { blocks: { type: Array, default: () => [] } },
  setup: props => () => {
    capturedGroupBlocks = props.blocks as readonly ChatUiAssistantMessageBlock[]
    return h('div', { 'data-testid': 'stub-block-group' })
  },
})

const ContextMenuItem = defineComponent({
  name: 'StubContextMenuItem',
  inheritAttrs: false,
  props: { disabled: Boolean },
  emits: ['select'],
  setup: (props, { emit, slots, attrs }) => () => h('button', {
    ...attrs,
    disabled: props.disabled,
    onClick: () => emit('select'),
  }, slots.default?.()),
})

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

const terminal: ChatUiTurnTerminalState = {
  status: 'failed',
  reason: 'provider_error',
  sourceMessageId: 'message-1',
  debugDetails: {
    statusCode: 500,
    code: 'E_PROVIDER',
  },
}

const mounted: App[] = []

afterEach(() => {
  while (mounted.length > 0) mounted.pop()?.unmount()
  document.body.replaceChildren()
  vi.useRealTimers()
})

function block(
  id: string,
  kind: ChatUiAssistantMessageBlock['kind'],
  overrides: Partial<ChatUiAssistantMessageBlock> = {},
): ChatUiAssistantMessageBlock {
  return {
    id,
    kind,
    status: 'complete',
    content: '',
    startedAt: null,
    endedAt: null,
    durationMs: null,
    ...overrides,
  }
}

async function mountMessage(options: {
  blocks?: ChatUiAssistantMessageBlock[]
  detailsExpanded?: boolean
  detailsToggleVisible?: boolean
  terminal?: ChatUiTurnTerminalState | null
  terminalLineText?: string
  terminalExplainText?: string
  toolCategories?: Record<string, ChatUiTimelineToolSummaryCategory>
  notificationText?: Record<string, string>
  onViewPlan?: () => Promise<void>
  chatUiHost?: ChatUiHost
} = {}) {
  const host = document.createElement('div')
  document.body.appendChild(host)
  const events = {
    toggleDetails: vi.fn(),
    toggleBlock: vi.fn(),
    toggleGroup: vi.fn(),
    previewFile: vi.fn(),
    openAgent: vi.fn(),
    addSelection: vi.fn(),
    copySuccess: vi.fn(),
    copyError: vi.fn(),
  }
  const app = createApp({
    setup() {
      return () => h(TimelineAssistantMessage, {
        sessionId: 'session-1',
        blocks: options.blocks ?? [],
        detailsExpanded: options.detailsExpanded ?? true,
        detailsToggleVisible: options.detailsToggleVisible ?? true,
        blockExpandedById: {},
        blockGroupExpandedById: {},
        notificationTextByBlockId: options.notificationText ?? {},
        subagentTargetByNotificationBlockId: {},
        toolCategoryByBlockId: options.toolCategories ?? {},
        terminal: options.terminal ?? null,
        terminalLineText: options.terminalLineText ?? '',
        terminalExplainText: options.terminalExplainText ?? '',
        canShowWorkDuration: true,
        workDurationMs: 1000,
        searchMatchesBySource: {},
        activeSearchMatchId: null,
        readOnly: false,
        labels,
        icons,
        buttonComponent: Button,
        tooltipComponent: passthrough('Tooltip'),
        tooltipTriggerComponent: passthrough('TooltipTrigger'),
        tooltipContentComponent: passthrough('TooltipContent'),
        contextMenuComponent: passthrough('ContextMenu'),
        contextMenuTriggerComponent: passthrough('ContextMenuTrigger'),
        contextMenuContentComponent: passthrough('ContextMenuContent'),
        contextMenuItemComponent: ContextMenuItem,
        blockGroupComponent: BlockGroupStub,
        thinkingItemComponent: testIdComponent('stub-thinking-item'),
        textItemComponent: testIdComponent('stub-text-item'),
        retryNoticeComponent: testIdComponent('stub-retry-notice'),
        toolCallItemComponent: testIdComponent('stub-tool-item'),
        mcpAppInlineAnchorComponent: emptyComponent('mcp-anchor'),
        generatedImageCardComponent: testIdComponent('stub-generated-image'),
        selectionPopoverComponent: emptyComponent('selection-popover'),
        providerIconComponent: emptyComponent('provider-icon'),
        isDark: false,
        codeBlockLightTheme: 'light',
        codeBlockDarkTheme: 'dark',
        codeBlockProps: {},
        codeBlockOptions: {},
        readBackgroundTaskNotice: () => null,
        getAgentModelLabel: () => null,
        onViewPlan: options.onViewPlan,
        onToggleDetails: events.toggleDetails,
        onToggleBlock: events.toggleBlock,
        onToggleBlockGroup: events.toggleGroup,
        onPreviewFile: events.previewFile,
        onOpenAgent: events.openAgent,
        onAddSelectionReference: events.addSelection,
        onCopySuccess: events.copySuccess,
        onCopyError: events.copyError,
      })
    },
  })
  if (options.chatUiHost !== undefined) app.provide(CHAT_UI_HOST, options.chatUiHost)
  mounted.push(app)
  app.mount(host)
  await nextTick()
  return { host, events }
}

describe('TimelineAssistantMessage', () => {
  it('renders the work summary and grouped work rows while expanded', async () => {
    const { host, events } = await mountMessage({
      blocks: [
        block('thinking-1', 'thinking', { content: 'reasoning' }),
        block('notify-1', 'notification', { content: 'notice' }),
        block('tool-1', 'tool', { toolName: 'read' }),
        block('text-1', 'text', { content: 'final answer' }),
      ],
      toolCategories: { 'tool-1': 'read' },
      notificationText: { 'notify-1': 'notice' },
    })

    expect(host.querySelector('[data-testid="timeline-work-summary"]')).not.toBeNull()
    expect(host.textContent).toContain('summary:none::1.0s')
    expect(host.querySelector('[data-testid="stub-block-group"]')).not.toBeNull()
    expect(capturedGroupBlocks.map(block => block.id)).toEqual(['thinking-1', 'notify-1', 'tool-1'])
    expect(host.querySelector('[data-testid="stub-text-item"]')).not.toBeNull()

    host.querySelector<HTMLButtonElement>('[data-testid="timeline-work-summary"] button')!.click()
    expect(events.toggleDetails).toHaveBeenCalledOnce()
  })

  it('hides the summary when details toggling is disabled', async () => {
    const { host } = await mountMessage({
      blocks: [block('text-1', 'text', { content: 'answer' })],
      detailsToggleVisible: false,
    })

    expect(host.querySelector('[data-testid="timeline-work-summary"]')).toBeNull()
  })

  it('renders and expands a failed terminal line with debug details and the plan action', async () => {
    const onViewPlan = vi.fn(async () => {})
    const terminal: ChatUiTurnTerminalState = {
      status: 'failed',
      reason: 'provider_error',
      sourceMessageId: 'message-1',
      debugDetails: {
        statusCode: 500,
        code: 'E_PROVIDER',
      },
      presentation: { action: 'view_plan' },
    }
    const { host, events } = await mountMessage({
      blocks: [],
      terminal,
      terminalLineText: 'Provider failed',
      terminalExplainText: 'Provider returned an error',
      onViewPlan,
    })

    expect(host.querySelector('[data-testid="timeline-terminal-line"]')).not.toBeNull()
    expect(host.textContent).toContain('Provider failed')
    expect(host.textContent).toContain('Provider returned an error')
    expect(host.textContent).toContain('Status Code')
    expect(host.textContent).toContain('E_PROVIDER')

    host.querySelector<HTMLButtonElement>('[data-testid="timeline-terminal-toggle"]')!.click()
    expect(events.toggleBlock).toHaveBeenCalledWith('terminal:message-1')

    const plan = host.querySelector<HTMLButtonElement>('[data-testid="dim-oauth-relay-view-plan"]')!
    plan.click()
    await nextTick()
    expect(onViewPlan).toHaveBeenCalledOnce()
  })

  it('copies the entire message through the host', async () => {
    const writeText = stubClipboard()
    const copy = vi.fn(async () => undefined)
    const { host, events } = await mountMessage({
      blocks: [block('text-1', 'text', { content: 'first' }), block('text-2', 'text', { content: 'second' })],
      chatUiHost: copyHost(copy),
    })

    host.querySelector<HTMLButtonElement>('[data-testid="timeline-assistant-copy-entire-message"]')!.click()
    await vi.waitFor(() => expect(events.copySuccess).toHaveBeenCalledOnce())
    expect(copy).toHaveBeenCalledWith('first\n\nsecond')
    expect(writeText).not.toHaveBeenCalled()
  })

  it('copies terminal debug details through the host and reports a rejected copy', async () => {
    const writeText = stubClipboard()
    const copy = vi.fn(async () => undefined)
    const { host, events } = await mountMessage({ terminal, chatUiHost: copyHost(copy) })
    const button = () => host.querySelector<HTMLButtonElement>('[data-testid="timeline-terminal-debug-copy"]')!

    button().click()
    await vi.waitFor(() => expect(events.copySuccess).toHaveBeenCalledOnce())
    expect(copy).toHaveBeenCalledWith('Status Code: 500\nCode: E_PROVIDER')
    expect(writeText).not.toHaveBeenCalled()
    await nextTick()
    expect(button().querySelector('[data-testid="icon-check"]')).not.toBeNull()

    copy.mockRejectedValueOnce(new Error('denied'))
    button().click()
    await vi.waitFor(() => expect(events.copyError).toHaveBeenCalledOnce())
    expect(events.copySuccess).toHaveBeenCalledOnce()
  })

  it('falls back to navigator.clipboard without a host', async () => {
    const writeText = stubClipboard()
    const { host, events } = await mountMessage({ terminal })

    host.querySelector<HTMLButtonElement>('[data-testid="timeline-terminal-debug-copy"]')!.click()
    await vi.waitFor(() => expect(events.copySuccess).toHaveBeenCalledOnce())
    expect(writeText).toHaveBeenCalledWith('Status Code: 500\nCode: E_PROVIDER')
  })
})
