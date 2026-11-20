<script setup lang="ts">
import {
  computed,
  onBeforeUnmount,
  onBeforeUpdate,
  onUpdated,
  ref,
  watch,
  type Component,
} from 'vue'
import MarkdownRender from 'markstream-vue'
import { useChatUiCopy } from './clipboard'
import TimelineDetailsRegion from './TimelineDetailsRegion.vue'
import TimelineSearchDomText from './TimelineSearchDomText.vue'
import TimelineSearchHighlightedText from './TimelineSearchHighlightedText.vue'
import type {
  ChatUiAssistantMessageAgentInfo,
  ChatUiAssistantMessageBlock,
  ChatUiAssistantMessageIcons,
  ChatUiAssistantMessageLabels,
  ChatUiAssistantMessagePerfMetrics,
  ChatUiTurnTerminalState,
} from './assistant-message'
import type {
  ChatUiBackgroundTaskNotice,
  ChatUiTimelineSubagentTarget,
  ChatUiTimelineToolSummaryCategory,
} from './block-group'
import { formatWorkDuration } from '../timeline/duration'
import {
  groupConsecutiveBlocks,
  isEmptyTimelineThinking,
  isGroupableWorkBlock,
  type ChatUiTimelineBlockGroup,
} from '../timeline/block-grouping'
import {
  timelineBlockSearchSourceId,
  timelineMessageSearchSourceId,
  type ChatUiTimelineSearchMatch,
} from '../timeline/search'
import { normalizeToolName } from '../timeline/tool-name'
import { getSelectedTextInElement } from '../utils/selection-copy'

defineOptions({
  name: 'TimelineAssistantMessage',
})

const EMPTY_SEARCH_MATCHES: ChatUiTimelineSearchMatch[] = []
const RUN_TERMINAL_HOOK_REJECTED_CODE = 'SESSION_RUN_ABORTED'
const DETAILS_ANIMATION_MS = 300

const props = withDefaults(defineProps<{
  sessionId?: string | null
  blocks?: readonly ChatUiAssistantMessageBlock[]
  model?: string
  detailsExpanded: boolean
  detailsToggleVisible: boolean
  blockExpandedById: Readonly<Record<string, boolean>>
  blockGroupExpandedById?: Readonly<Record<string, boolean>>
  notificationTextByBlockId?: Readonly<Record<string, string>>
  subagentTargetByNotificationBlockId?: Readonly<Record<string, ChatUiTimelineSubagentTarget>>
  toolCategoryByBlockId?: Readonly<Record<string, ChatUiTimelineToolSummaryCategory>>
  terminal?: ChatUiTurnTerminalState | null
  terminalLineText?: string
  terminalExplainText?: string
  canShowWorkDuration?: boolean
  workDurationMs?: number | null
  turnStartedAt?: Date | null
  elapsedNowMs?: number
  searchQuery?: string
  searchMatchesBySource?: Readonly<Record<string, readonly ChatUiTimelineSearchMatch[]>>
  activeSearchMatchId?: string | null
  markdownViewportPriority?: boolean
  isRunning?: boolean
  readOnly?: boolean
  mcpAppInlineAnchorsEnabled?: boolean
  selectionEnabled?: boolean
  labels: ChatUiAssistantMessageLabels
  icons: ChatUiAssistantMessageIcons
  buttonComponent: Component
  tooltipComponent: Component
  tooltipTriggerComponent: Component
  tooltipContentComponent: Component
  contextMenuComponent: Component
  contextMenuTriggerComponent: Component
  contextMenuContentComponent: Component
  contextMenuItemComponent: Component
  blockGroupComponent: Component
  thinkingItemComponent: Component
  textItemComponent: Component
  retryNoticeComponent: Component
  toolCallItemComponent: Component
  mcpAppInlineAnchorComponent: Component
  generatedImageCardComponent: Component
  selectionPopoverComponent: Component
  providerIconComponent: Component
  mediaComponent?: Component
  markdownComponent?: Component
  isDark: boolean
  codeBlockLightTheme: string
  codeBlockDarkTheme: string
  codeBlockProps: Readonly<Record<string, unknown>>
  codeBlockOptions: Readonly<Record<string, unknown>>
  readBackgroundTaskNotice: (block: ChatUiAssistantMessageBlock) => ChatUiBackgroundTaskNotice | null
  getAgentModelLabel: (info: ChatUiAssistantMessageAgentInfo) => string | null
  getRetryRunId?: (block: ChatUiAssistantMessageBlock) => string | undefined
  loadExecOutput?: (block: ChatUiAssistantMessageBlock, outputPath: string) => Promise<string>
  onViewPlan?: () => Promise<void> | void
  onCopySuccess?: () => void
  onCopyError?: () => void
  onRenderStart?: () => void
  onRenderFinish?: (metrics: ChatUiAssistantMessagePerfMetrics) => void
}>(), {
  sessionId: null,
  blocks: () => [],
  model: undefined,
  blockGroupExpandedById: () => ({}),
  notificationTextByBlockId: () => ({}),
  subagentTargetByNotificationBlockId: () => ({}),
  toolCategoryByBlockId: () => ({}),
  terminal: null,
  terminalLineText: '',
  terminalExplainText: '',
  canShowWorkDuration: false,
  workDurationMs: null,
  turnStartedAt: null,
  elapsedNowMs: undefined,
  searchQuery: '',
  searchMatchesBySource: () => ({}),
  activeSearchMatchId: null,
  markdownViewportPriority: true,
  isRunning: false,
  readOnly: false,
  mcpAppInlineAnchorsEnabled: true,
  selectionEnabled: true,
  mediaComponent: undefined,
  markdownComponent: undefined,
  getRetryRunId: undefined,
  loadExecOutput: undefined,
  onViewPlan: undefined,
  onCopySuccess: undefined,
  onCopyError: undefined,
  onRenderStart: undefined,
  onRenderFinish: undefined,
})

const emit = defineEmits<{
  'toggle-details': []
  'toggle-block': [blockId: string]
  'toggle-block-group': [groupId: string]
  'preview-file': [target: { path: string; startLine?: number }]
  'open-agent': [target: ChatUiTimelineSubagentTarget]
  'add-selection-reference': [quote: string, comment: string]
}>()

const rootRef = ref<HTMLElement | null>(null)
const selectedText = ref('')
const selectionQuote = ref<string | null>(null)
const selectionAnchor = ref({ x: 0, y: 0 })
const selectionPopoverOpen = ref(false)

function handleMessageSelection(event: PointerEvent) {
  if (!props.selectionEnabled) return
  const root = rootRef.value
  const selection = window.getSelection()
  if (
    !root
    || !selection
    || selection.isCollapsed
    || !root.contains(selection.anchorNode)
    || !root.contains(selection.focusNode)
  ) return
  const quote = selection.toString()
  if (!quote) return
  selectionQuote.value = quote
  selectionAnchor.value = { x: event.clientX, y: event.clientY }
  selectionPopoverOpen.value = true
}

function closeSelectionPopover() {
  selectionPopoverOpen.value = false
  selectionQuote.value = null
}

function addMessageReference(comment: string) {
  const quote = selectionQuote.value
  if (!quote) return
  emit('add-selection-reference', quote, comment)
  closeSelectionPopover()
}

const firstBlockAnchorId = computed(() => props.blocks[0]?.id)
const assistantNodeIndex = computed(() => firstBlockAnchorId.value ? `assistant:${firstBlockAnchorId.value}` : undefined)
const workSummaryNodeIndex = computed(() => firstBlockAnchorId.value ? `assistant-summary:${firstBlockAnchorId.value}` : undefined)

const priorityMarkdownPerformanceProps = {
  deferNodesUntilVisible: true,
  initialRenderBatchSize: 16,
  renderBatchSize: 16,
  renderBatchDelay: 16,
  renderBatchBudgetMs: 4,
  renderBatchIdleTimeoutMs: 120,
  maxLiveNodes: 480,
  liveNodeBuffer: 96,
} as const
const backgroundMarkdownPerformanceProps = {
  deferNodesUntilVisible: true,
  initialRenderBatchSize: 4,
  renderBatchSize: 4,
  renderBatchDelay: 48,
  renderBatchBudgetMs: 2,
  renderBatchIdleTimeoutMs: 300,
  maxLiveNodes: 160,
  liveNodeBuffer: 24,
} as const
const streamingMarkdownPerformanceProps = {
  smoothStreaming: true,
  deferNodesUntilVisible: false,
  initialRenderBatchSize: 512,
  renderBatchSize: 512,
  renderBatchDelay: 0,
} as const

const workBlocks = computed(() => props.blocks.filter(block => (
  block.kind !== 'media'
  && block.kind !== 'generated-image'
  && block.kind !== 'plan-draft'
  && block.kind !== 'notification'
  && block.kind !== 'retry'
)))

// ── Subagent / exec notifications ─────────────────────────

function readSubagentNoticeParams(block: ChatUiAssistantMessageBlock): ChatUiBackgroundTaskNotice | null {
  const params = props.readBackgroundTaskNotice(block)
  if (!params || params.toolName !== 'agent' || params.agentName === undefined) return null
  return params
}

function readExecTaskNoticeParams(block: ChatUiAssistantMessageBlock): ChatUiBackgroundTaskNotice | null {
  const params = props.readBackgroundTaskNotice(block)
  if (!params) return null
  if (params.toolName === 'agent' && params.agentName !== undefined) return null
  return params
}

function isSubagentNotificationBlock(block: ChatUiAssistantMessageBlock): boolean {
  return block.kind === 'notification' && readSubagentNoticeParams(block) !== null
}

function isExecTaskNotificationBlock(block: ChatUiAssistantMessageBlock): boolean {
  return block.kind === 'notification' && readExecTaskNoticeParams(block) !== null
}

function execTaskNoticeParams(block: ChatUiAssistantMessageBlock): ChatUiBackgroundTaskNotice | null {
  return readExecTaskNoticeParams(block)
}

function execTaskStatusText(block: ChatUiAssistantMessageBlock): string {
  const params = readExecTaskNoticeParams(block)
  if (!params) return ''
  return props.labels.backgroundTaskStatus[params.status]
}

const execOutputCache = ref<Record<string, { content: string; loading: boolean; error?: string }>>({})

async function loadExecOutput(block: ChatUiAssistantMessageBlock) {
  const params = readExecTaskNoticeParams(block)
  if (!props.loadExecOutput || !params?.outputPath || !props.sessionId) return
  if (execOutputCache.value[block.id]) return

  execOutputCache.value[block.id] = { content: '', loading: true }
  try {
    const content = await props.loadExecOutput(block, params.outputPath)
    execOutputCache.value[block.id] = { content, loading: false }
  } catch (e) {
    execOutputCache.value[block.id] = { content: '', loading: false, error: e instanceof Error ? e.message : String(e) }
  }
}

function toggleExecOutput(block: ChatUiAssistantMessageBlock) {
  const cached = execOutputCache.value[block.id]
  if (!cached) {
    void loadExecOutput(block)
  }
  emit('toggle-block', block.id)
}

function subagentNoticeParams(block: ChatUiAssistantMessageBlock): ChatUiBackgroundTaskNotice | null {
  return readSubagentNoticeParams(block)
}

function agentModelLabel(block: ChatUiAssistantMessageBlock): string | null {
  return props.getAgentModelLabel(subagentNoticeParams(block) ?? {})
}

function hasSubagentSummary(block: ChatUiAssistantMessageBlock): boolean {
  return subagentSummary(block).length > 0
}

function subagentSummary(block: ChatUiAssistantMessageBlock): string {
  const match = block.content.match(/<result[^>]*>([\s\S]*?)<\/result>/)
  if (match?.[1]) {
    return match[1].trim()
  }
  const params = readSubagentNoticeParams(block)
  return params?.summary?.trim() ?? ''
}

function requiredNotificationText(block: ChatUiAssistantMessageBlock): string {
  const text = props.notificationTextByBlockId[block.id]
  if (text === undefined) {
    throw new Error(`Timeline notification block requires formatted text: ${block.id}`)
  }
  return text
}

const statusBlocks = computed(() => workBlocks.value.filter(block => block.kind === 'status'))
const toolCategoriesInWorkOrder = computed(() => {
  const categories: ChatUiTimelineToolSummaryCategory[] = []
  for (const block of workBlocks.value) {
    if (block.kind !== 'tool') continue
    const category = props.toolCategoryByBlockId[block.id] ?? 'unknown'
    if (!categories.includes(category)) categories.push(category)
  }
  return categories
})
const workActionSummaryText = computed(() => {
  const categories = toolCategoriesInWorkOrder.value
  if (categories.length === 0) {
    return ''
  }
  const actionText = categories
    .slice(0, 3)
    .map(action => props.labels.workAction[action])
    .join(props.labels.workActionSeparator)
  return categories.length > 3
    ? `${actionText}${props.labels.workActionMoreSuffix}`
    : actionText
})
// The row a collapsed turn keeps on screen is the message's closing text — the
// answer to read once the work is folded away.
//
// A message can end with work instead of prose: a run stopped mid-turn, or a
// turn parked on a blocking tool call (an AskUserQuestion awaiting its answer).
// Those messages have no closing text, and taking the last content block
// literally used to collapse *every* text block of the message away, so the
// assistant's words disappeared from the timeline while the collapsed work
// summary kept naming the tool. Fall back to the message's last text block:
// the collapse hides work, never the prose of the turn.
const finalVisibleTextBlock = computed(() => {
  for (let index = workBlocks.value.length - 1; index >= 0; index -= 1) {
    const block = workBlocks.value[index]!
    if (block.kind === 'text') {
      return block
    }
  }
  return null
})
// With a turn terminal present, the message-level status block is redundant —
// the terminal line carries the failure/interrupt state.
const suppressMessageStatusBlocks = computed(() => hasTurnTerminal.value)
const renderBlocksBase = computed(() => props.blocks.filter(block =>
  !(suppressMessageStatusBlocks.value && block.kind === 'status'),
))
// Tool blocks the grouping can never swallow: an explicit inline MCP App keeps
// its own visible surface and stays a single boundary (timeline-helpers'
// isGroupableWorkBlock is the single source for that rule).
function groupingShape(block: ChatUiAssistantMessageBlock) {
  return {
    id: block.id,
    kind: block.kind,
    toolSourceKind: block.toolSource?.kind === 'mcp' ? 'mcp' as const : undefined,
    displayMode: block.toolSource?.displayMode,
    isEmptyThinking: isEmptyTimelineThinking(block),
  }
}

function groupBlocks(blocks: readonly ChatUiAssistantMessageBlock[]) {
  return groupConsecutiveBlocks([...blocks], { shape: groupingShape })
}

function isGroupingBoundaryToolBlock(block: ChatUiAssistantMessageBlock): boolean {
  return block.kind === 'tool' && !isGroupableWorkBlock(groupingShape(block))
}
// Block ids a work run swallows (work blocks plus the notifications absorbed
// into a run). `groupConsecutiveBlocks` is the same grouping the render path
// below uses, so a row a group can hide is never treated as always-visible.
const groupedBlockIds = computed(() => {
  const ids = new Set<string>()
  for (const group of groupBlocks(renderBlocksBase.value)) {
    if (group.kind !== 'group') {
      continue
    }
    for (const block of group.blocks) {
      ids.add(block.id)
    }
  }
  return ids
})
// Rows the collapsed turn keeps on screen. Only rows a work group can never
// swallow may stay: collapsing the turn must not surface a row out of a hidden
// group, which is what an absorbed notification or a fullscreen MCP tool would
// do (both are groupable, so the group already hides them).
const collapsedBlocks = computed(() => renderBlocksBase.value.filter(block =>
  (block.kind === 'media'
    || block.kind === 'generated-image'
    || block.kind === 'plan-draft'
    || block.kind === 'notification'
    || block.kind === 'retry'
    || block.kind === 'status'
    || block.id === finalVisibleTextBlock.value?.id
    || isGroupingBoundaryToolBlock(block))
  && !groupedBlockIds.value.has(block.id),
))
const collapsedBlockIds = computed(() => new Set(collapsedBlocks.value.map(block => block.id)))
const hasHiddenBlocks = computed(() => props.blocks.some(block => !collapsedBlockIds.value.has(block.id)))
const showWorkSummaryToggle = computed(() => hasHiddenBlocks.value && props.detailsToggleVisible)

// Turn details expand/collapse. `detailsExpanded` (the prop) is the truth for
// the toggle affordance, `workDetailsOpen` drives the collapse-grid classes of
// the collapsible rows and `workDetailsMounted` decides which blocks render:
// the row set only changes once the rows have finished animating, so the swap
// never lands in the same frame as the state flip. The collapse latch keeps
// the rows mounted while the prop is already false.
// Same duration as the region's collapse-grid transition (TimelineDetailsRegion).
const detailsCollapseLatch = ref(false)
const workDetailsOpen = ref(props.detailsExpanded)
const workDetailsMounted = computed(() => props.detailsExpanded || detailsCollapseLatch.value)
let detailsCommitTimer: ReturnType<typeof setTimeout> | undefined
let detailsOpenFrame: number | undefined

function cancelDetailsAnimation() {
  if (detailsCommitTimer !== undefined) {
    clearTimeout(detailsCommitTimer)
    detailsCommitTimer = undefined
  }
  if (detailsOpenFrame !== undefined) {
    cancelAnimationFrame(detailsOpenFrame)
    detailsOpenFrame = undefined
  }
}

watch(() => props.detailsExpanded, (value, previous) => {
  cancelDetailsAnimation()
  const rowsWereMounted = previous === true || detailsCollapseLatch.value
  if (value) {
    detailsCollapseLatch.value = false
    if (rowsWereMounted) {
      workDetailsOpen.value = true
      return
    }
    workDetailsOpen.value = false
    detailsOpenFrame = requestAnimationFrame(() => {
      detailsOpenFrame = requestAnimationFrame(() => {
        detailsOpenFrame = undefined
        workDetailsOpen.value = true
      })
    })
    return
  }
  workDetailsOpen.value = false
  if (!hasHiddenBlocks.value || !rowsWereMounted) {
    return
  }
  detailsCollapseLatch.value = true
  detailsCommitTimer = setTimeout(() => {
    detailsCommitTimer = undefined
    detailsCollapseLatch.value = false
  }, DETAILS_ANIMATION_MS)
})
onBeforeUnmount(cancelDetailsAnimation)

const renderBlocks = computed(() => workDetailsMounted.value || !hasHiddenBlocks.value
  ? renderBlocksBase.value
  : collapsedBlocks.value)
let cachedBlockGroups = new Map<string, Extract<ChatUiTimelineBlockGroup<ChatUiAssistantMessageBlock>, { kind: 'group' }>>()
const renderBlockGroups = computed(() => {
  if (!workDetailsMounted.value && hasHiddenBlocks.value) {
    return renderBlocks.value.map(block => ({ kind: 'single' as const, block }))
  }
  const nextGroupCache = new Map<string, Extract<ChatUiTimelineBlockGroup<ChatUiAssistantMessageBlock>, { kind: 'group' }>>()
  const groups = groupBlocks(renderBlocks.value).map((group) => {
    if (group.kind === 'single') {
      return group
    }
    const cached = cachedBlockGroups.get(group.groupId)
    const stableGroup = cached
      && cached.blocks.length === group.blocks.length
      && group.blocks.every((block, index) => cached.blocks[index] === block)
      ? cached
      : group
    nextGroupCache.set(group.groupId, stableGroup)
    return stableGroup
  })
  cachedBlockGroups = nextGroupCache
  return groups
})
// Rows that only exist while the turn details are expanded: a group is always
// one of them, a single row is one unless it is a residual block that the
// collapsed turn keeps on screen (final text / image / plan / status /
// notification / MCP tool).
function isCollapsibleDetailsItem(item: ChatUiTimelineBlockGroup<ChatUiAssistantMessageBlock>): boolean {
  return item.kind === 'group' || !collapsedBlockIds.value.has(item.block.id)
}
const terminalStatusType = computed(() => statusBlocks.value.at(-1)?.statusType ?? null)
const turnTerminalStatusType = computed<'error' | 'cancelled' | null>(() => {
  if (props.terminal === null) return null
  return props.terminal.status === 'failed' ? 'error' : 'cancelled'
})
const workSummaryText = computed(() => {
  const actionSummaryText = props.detailsExpanded ? '' : workActionSummaryText.value
  const durationText = props.canShowWorkDuration && props.workDurationMs !== null
    ? formatWorkDuration(props.workDurationMs)
    : null
  return props.labels.workSummary({
    terminalStatus: turnTerminalStatusType.value,
    actionsText: actionSummaryText,
    durationText,
    detailsExpanded: props.detailsExpanded,
  })
})
function hasTextContent(content: string): boolean {
  return /\S/.test(content)
}

// ── Turn terminal line (run_terminal / user_interrupt state) ──────────────

const hasTurnTerminal = computed(() => props.terminal !== null && props.terminal !== undefined)

const turnTerminalFailed = computed(() => props.terminal?.status === 'failed'
  && props.terminal.code !== RUN_TERMINAL_HOOK_REJECTED_CODE)

const turnTerminalExpandable = computed(() => {
  if (!hasTurnTerminal.value) return false
  return props.terminal!.reason !== 'user_interrupt'
})

const turnTerminalExpanded = computed(() => {
  if (!hasTurnTerminal.value) return false
  return props.blockExpandedById[`terminal:${props.terminal!.sourceMessageId}`] ?? false
})

const turnTerminalIcon = computed(() => props.terminal?.status === 'failed'
  ? props.icons.triangleAlert
  : props.icons.octagonPause)

const turnTerminalLineText = computed(() => props.terminalLineText)
const turnTerminalExplainText = computed(() => props.terminalExplainText)
const turnTerminalSearchMatches = computed(() => props.terminal
  ? props.searchMatchesBySource[timelineMessageSearchSourceId(props.terminal.sourceMessageId)] ?? EMPTY_SEARCH_MATCHES
  : EMPTY_SEARCH_MATCHES)

const hasTurnTerminalPlanAction = computed(() => {
  if (!hasTurnTerminal.value) return false
  return props.terminal!.presentation?.action === 'view_plan'
})

const turnTerminalPlanActionPending = ref(false)

async function openTurnTerminalPlanAction(): Promise<void> {
  if (turnTerminalPlanActionPending.value) return
  turnTerminalPlanActionPending.value = true
  try {
    await props.onViewPlan?.()
  } finally {
    turnTerminalPlanActionPending.value = false
  }
}

const turnTerminalDebugLines = computed(() => {
  if (!hasTurnTerminal.value) return []
  const details = props.terminal!.debugDetails
  if (details === undefined) return []
  const lines: Array<{ key: string; label: string; value: string }> = []
  appendDebugDetailLine(lines, 'statusCode', 'Status Code', details.statusCode)
  appendDebugDetailLine(lines, 'status', 'Status', details.status)
  appendDebugDetailLine(lines, 'code', 'Code', details.code)
  appendDebugDetailLine(lines, 'providerCode', 'Provider Code', details.providerCode)
  appendDebugDetailLine(lines, 'requestId', 'Request ID', details.requestId)
  appendDebugDetailLine(lines, 'message', 'Message', [
    details.providerUrl === undefined ? undefined : `Provider URL: ${details.providerUrl}`,
    details.modelId === undefined ? undefined : `Model ID: ${details.modelId}`,
    details.message,
  ].filter((value): value is string => value !== undefined).join('\n'))
  appendDebugDetailLine(lines, 'responseBody', 'Response Body', details.responseBody)
  if (details.responseHeaders !== undefined) {
    appendDebugDetailLine(
      lines,
      'responseHeaders',
      'Response Headers',
      Object.entries(details.responseHeaders)
        .map(([key, value]) => `${key}: ${value}`)
        .join('\n'),
    )
  }
  return lines
})

const turnTerminalRuntimeHostText = computed(() => {
  const runtimeHost = props.terminal?.runtimeHost
  if (runtimeHost === undefined) return ''
  return `${runtimeHost.platform}/${runtimeHost.arch}/${runtimeHost.appVersion}/${runtimeHost.osVersion}`
})

function appendDebugDetailLine(
  lines: Array<{ key: string; label: string; value: string }>,
  key: string,
  label: string,
  value: string | number | undefined,
): void {
  if (typeof value === 'number') {
    lines.push({ key, label, value: String(value) })
    return
  }
  if (typeof value !== 'string') return
  if (value === '') return
  lines.push({ key, label, value })
}

const copy = useChatUiCopy()
const turnTerminalDebugCopied = ref(false)

async function copyTurnTerminalDebugDetails(): Promise<void> {
  if (!hasTurnTerminal.value) return
  const runtimeHostText = turnTerminalRuntimeHostText.value
  const text = [
    ...(runtimeHostText === '' ? [] : [runtimeHostText]),
    ...turnTerminalDebugLines.value.map(line => `${line.label}: ${line.value}`),
  ].join('\n')
  try {
    await copy(text)
    turnTerminalDebugCopied.value = true
    props.onCopySuccess?.()
    setTimeout(() => {
      turnTerminalDebugCopied.value = false
    }, 1500)
  } catch {
    props.onCopyError?.()
  }
}

function blockCopyText(block: ChatUiAssistantMessageBlock): string | undefined {
  if (block.kind === 'text') {
    return block.content
  }
  if (block.kind === 'status') {
    const content = statusBlockText(block)
    return content ? content : undefined
  }
  if (block.kind === 'generated-image') {
    return block.content
  }
  if (block.kind === 'plan-draft') {
    return block.content
  }
  return undefined
}

const hasRunningBlock = computed(() => props.blocks.some(block => block.status === 'running'))
const isMessageFinal = computed(() => !props.isRunning && !hasRunningBlock.value)
const hasCopyPayload = computed(() => isMessageFinal.value && props.blocks.some((block) => {
  const content = blockCopyText(block)
  return content !== undefined && hasTextContent(content)
}))
const copyPayload = computed(() => {
  const parts: string[] = []
  for (const block of props.blocks) {
    const content = blockCopyText(block)
    if (content !== undefined) {
      parts.push(content)
    }
  }
  return parts.join('\n\n')
})
const statusValue = computed(() => terminalStatusType.value ?? (props.isRunning || hasRunningBlock.value ? 'streaming' : 'complete'))
const hasSelectedText = computed(() => selectedText.value.trim().length > 0)
const markdownPerformanceProps = computed(() =>
  props.markdownViewportPriority
    ? priorityMarkdownPerformanceProps
    : backgroundMarkdownPerformanceProps,
)

onBeforeUpdate(() => {
  props.onRenderStart?.()
})

onUpdated(() => {
  props.onRenderFinish?.({
    blockCount: props.blocks.length,
    textLength: props.blocks.reduce((total, block) => total + block.content.length, 0),
    status: statusValue.value,
    viewportPriority: props.markdownViewportPriority,
  })
})

function statusBlockText(block: ChatUiAssistantMessageBlock): string {
  const content = block.content.trim()
  if (content) {
    return content
  }
  if (block.statusType === 'error') {
    return props.labels.runFailedPlaceholder
  }
  if (block.statusType === 'cancelled') {
    return props.labels.userInterrupt
  }
  return ''
}

async function copyText(content: string) {
  await copy(content)
  props.onCopySuccess?.()
}

async function copySelectedText() {
  if (!hasSelectedText.value) return
  await copyText(selectedText.value)
}

function refreshSelectedText() {
  selectedText.value = getSelectedTextInElement(rootRef.value)
}

function handleContextMenuOpen(open: boolean) {
  if (open) {
    refreshSelectedText()
  }
}

function toggleDetailsExpanded() {
  emit('toggle-details')
}

function toggleBlock(blockId: string) {
  emit('toggle-block', blockId)
}

function markdownIndexKey(block: ChatUiAssistantMessageBlock) {
  return `${props.sessionId ?? 'session'}:${block.id}`
}

function markdownIndexKeyBinding(block: ChatUiAssistantMessageBlock) {
  const indexKey = markdownIndexKey(block)
  return { indexKey }
}

function markdownRenderKey(block: ChatUiAssistantMessageBlock): string {
  return block.id
}

function basename(path: string | undefined): string {
  if (!path) {
    return ''
  }
  return path.split(/[\\/]/).filter(Boolean).at(-1) ?? path
}

function planDraftMeta(block: ChatUiAssistantMessageBlock): string {
  const parts = [
    block.planDraft?.mode,
    basename(block.planDraft?.path),
  ].filter((part): part is string => Boolean(part))
  return parts.join(' · ')
}

function isBrowserScreenshotImageBlock(block: ChatUiAssistantMessageBlock): boolean {
  return normalizeToolName(block.toolName) === 'browser'
}

type MarkdownRenderBindingsCacheEntry = {
  viewportPriority: boolean
  bindings: Record<string, unknown>
}
const markdownRenderBindingsCache = new WeakMap<ChatUiAssistantMessageBlock, MarkdownRenderBindingsCacheEntry>()

function markdownRenderBindings(block: ChatUiAssistantMessageBlock) {
  const cached = markdownRenderBindingsCache.get(block)
  if (cached && cached.viewportPriority === props.markdownViewportPriority) {
    return cached.bindings
  }

  const bindings = {
    ...markdownPerformanceProps.value,
    ...(block.status === 'running' ? streamingMarkdownPerformanceProps : {}),
    ...markdownIndexKeyBinding(block),
    viewportPriority: props.markdownViewportPriority,
  }
  markdownRenderBindingsCache.set(block, {
    viewportPriority: props.markdownViewportPriority,
    bindings,
  })
  return bindings
}

function isMcpAppToolBlock(block: ChatUiAssistantMessageBlock): boolean {
  return block.kind === 'tool' && block.toolSource?.kind === 'mcp'
}

function retryRunId(block: ChatUiAssistantMessageBlock): string | undefined {
  return props.getRetryRunId?.(block)
}
</script>

<template>
  <component :is="contextMenuComponent" @update:open="handleContextMenuOpen">
    <component :is="contextMenuTriggerComponent" as-child>
      <div
        ref="rootRef"
        class="group min-w-0 outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
        tabindex="0"
        data-testid="timeline-assistant-message"
        :data-message-status="statusValue"
        :data-node-index="assistantNodeIndex"
        @contextmenu.capture="refreshSelectedText"
        @pointerup="handleMessageSelection"
      >
        <div
          v-if="showWorkSummaryToggle"
          class="my-2 grid min-h-7 w-full grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-center gap-2"
          data-testid="timeline-work-summary"
          :data-expanded="props.detailsExpanded ? 'true' : 'false'"
          :data-node-index="workSummaryNodeIndex"
        >
          <span class="h-px min-w-0 bg-border" aria-hidden="true" />
          <button
            type="button"
            class="inline-grid h-7 grid-cols-[1fr_14px] items-center gap-1.5 whitespace-nowrap rounded px-2 text-base hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
            :class="turnTerminalFailed ? 'text-activity-warning' : 'text-muted-foreground hover:text-foreground'"
            :aria-expanded="props.detailsExpanded"
            @click="toggleDetailsExpanded"
          >
            <span class="leading-none">{{ workSummaryText }}</span>
            <component :is="icons.chevron" class="h-3.5 w-3.5 transition-transform" :class="{ 'rotate-90': props.detailsExpanded }" />
          </button>
          <span class="h-px min-w-0 bg-border" aria-hidden="true" />
        </div>

        <div class="mb-2" data-testid="timeline-blocks-container">
          <template v-for="group in renderBlockGroups" :key="group.kind === 'group' ? group.groupId : group.block.id">
            <TimelineDetailsRegion
              :collapsible="isCollapsibleDetailsItem(group)"
              :open="workDetailsOpen"
            >
              <component
                :is="blockGroupComponent"
                v-if="group.kind === 'group'"
                :group-id="group.groupId"
                :blocks="group.blocks"
                :expanded="blockGroupExpandedById[group.groupId] ?? false"
                :block-expanded-by-id="blockExpandedById"
                :notification-text-by-block-id="notificationTextByBlockId"
                :subagent-target-by-notification-block-id="subagentTargetByNotificationBlockId"
                :elapsed-now-ms="elapsedNowMs"
                :session-id="sessionId ?? null"
                :mcp-app-inline-anchors-enabled="mcpAppInlineAnchorsEnabled"
                @toggle-block="toggleBlock"
                @toggle-block-group="groupId => emit('toggle-block-group', groupId)"
                @open-agent="emit('open-agent', $event)"
                @preview-file="emit('preview-file', $event)"
              />
              <component
                :is="thinkingItemComponent"
                v-else-if="group.kind === 'single' && group.block.kind === 'thinking'"
                :block="group.block"
                :expanded="blockExpandedById[group.block.id] ?? false"
                :elapsed-started-at="group.block.startedAt"
                :elapsed-now-ms="elapsedNowMs"
                @toggle="toggleBlock(group.block.id)"
              />
              <component
                :is="textItemComponent"
                v-else-if="group.kind === 'single' && group.block.kind === 'text'"
                :block="group.block"
                :session-id="sessionId ?? null"
                :search-query="searchQuery"
                :search-matches="searchMatchesBySource[timelineBlockSearchSourceId(group.block.id)] ?? EMPTY_SEARCH_MATCHES"
                :active-search-match-id="activeSearchMatchId"
                :markdown-viewport-priority="markdownViewportPriority"
              />
              <component
                :is="mediaComponent"
                v-else-if="group.kind === 'single' && group.block.kind === 'media' && mediaComponent"
                :block="group.block"
              />
              <component
                :is="retryNoticeComponent"
                v-else-if="group.kind === 'single' && group.block.kind === 'retry' && group.block.retryNotice"
                :notice="group.block.retryNotice"
                :active="isRunning"
                :session-id="sessionId"
                :run-id="retryRunId(group.block)"
                :read-only="readOnly"
              />
              <!-- Subagent notification: agent chip + status + expandable summary -->
              <div
                v-else-if="group.kind === 'single' && group.block.kind === 'notification' && isSubagentNotificationBlock(group.block)"
                class="mb-3"
                data-testid="timeline-notification-block"
                :data-block-id="group.block.id"
              >
                <component
                  :is="buttonComponent"
                  variant="ghost"
                  class="group/tool-call !p-0 h-auto min-w-0 flex-1 justify-start gap-1 rounded-none text-base font-normal shadow-none transition-colors hover:bg-transparent dark:hover:bg-transparent"
                  :class="{ 'cursor-default': !subagentTargetByNotificationBlockId[group.block.id] && !hasSubagentSummary(group.block) }"
                  :data-testid="subagentTargetByNotificationBlockId[group.block.id] ? 'timeline-subagent-chat-open' : undefined"
                  @click="subagentTargetByNotificationBlockId[group.block.id]
                    ? emit('open-agent', subagentTargetByNotificationBlockId[group.block.id]!)
                    : hasSubagentSummary(group.block) && emit('toggle-block', group.block.id)"
                >
                  <component :is="icons.bot" class="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-colors group-hover/tool-call:text-foreground mr-0.5" />
                  <component :is="tooltipComponent">
                    <component :is="tooltipTriggerComponent" as-child>
                      <span class="inline-flex shrink-0 items-center gap-1 rounded-md bg-foreground/[0.06] px-1.5 py-0.5">
                        <component
                          :is="providerIconComponent"
                          :provider-id="subagentNoticeParams(group.block)?.providerId"
                          :model-id="subagentNoticeParams(group.block)?.modelId"
                          :external-agent-type="subagentNoticeParams(group.block)?.externalAgentType"
                          :size="14"
                        />
                        <span class="text-sm font-medium text-foreground">{{ subagentNoticeParams(group.block)?.agentName }}</span>
                      </span>
                    </component>
                    <component :is="tooltipContentComponent" v-if="agentModelLabel(group.block)">
                      {{ agentModelLabel(group.block) }}
                    </component>
                  </component>
                  <span class="text-base text-muted-foreground">
                    {{ requiredNotificationText(group.block) }}
                  </span>
                  <component
                    :is="icons.chevron"
                    v-if="subagentTargetByNotificationBlockId[group.block.id] || hasSubagentSummary(group.block)"
                    class="h-4 w-4 shrink-0 text-muted-foreground transition-transform group-hover/tool-call:text-foreground"
                    :class="{
                      'rotate-90': !subagentTargetByNotificationBlockId[group.block.id]
                        && (blockExpandedById[group.block.id] ?? false),
                    }"
                  />
                </component>
                <div
                  v-if="!subagentTargetByNotificationBlockId[group.block.id] && hasSubagentSummary(group.block)"
                  class="collapse-grid"
                  :class="{ 'collapse-grid-open': blockExpandedById[group.block.id] ?? false }"
                >
                  <div class="collapse-grid-inner">
                    <div class="py-1">
                      <div class="mt-1 max-h-[240px] overflow-auto rounded-md border border-border bg-card">
                        <pre class="px-3 py-2 font-mono text-xs leading-5 text-foreground/80 whitespace-pre-wrap break-words">{{ subagentSummary(group.block) }}</pre>
                      </div>
                    </div>
                  </div>
                </div>
              </div>
              <!-- Exec background task notification -->
              <div
                v-else-if="group.kind === 'single' && group.block.kind === 'notification' && isExecTaskNotificationBlock(group.block)"
                class="mb-3"
                data-testid="timeline-notification-block"
                :data-block-id="group.block.id"
              >
                <component
                  :is="buttonComponent"
                  variant="ghost"
                  class="group/tool-call !p-0 h-auto min-w-0 flex-1 justify-start gap-1 rounded-none text-base font-normal shadow-none transition-colors hover:bg-transparent dark:hover:bg-transparent"
                  @click="toggleExecOutput(group.block)"
                >
                  <component :is="icons.terminal" class="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-colors group-hover/tool-call:text-foreground mr-0.5" />
                  <span class="shrink-0 text-muted-foreground transition-colors group-hover/tool-call:text-foreground">{{ execTaskNoticeParams(group.block)?.label ?? labels.backgroundTaskCommand }}</span>
                  <span class="shrink-0 text-muted-foreground/70">{{ execTaskStatusText(group.block) }}</span>
                  <component
                    :is="icons.chevron"
                    v-if="execTaskNoticeParams(group.block)?.outputPath"
                    class="h-4 w-4 shrink-0 text-muted-foreground transition-transform group-hover/tool-call:text-foreground"
                    :class="{ 'rotate-90': blockExpandedById[group.block.id] ?? false }"
                  />
                </component>
                <div
                  v-if="execTaskNoticeParams(group.block)?.outputPath"
                  class="collapse-grid"
                  :class="{ 'collapse-grid-open': blockExpandedById[group.block.id] ?? false }"
                >
                  <div class="collapse-grid-inner">
                    <div class="py-1">
                      <div class="mt-1 max-h-[240px] overflow-auto rounded-md border border-border bg-card">
                        <div v-if="execOutputCache[group.block.id]?.loading" class="px-3 py-2 text-xs text-muted-foreground">Loading...</div>
                        <div v-else-if="execOutputCache[group.block.id]?.error" class="px-3 py-2 text-xs text-activity-warning">{{ execOutputCache[group.block.id].error }}</div>
                        <pre v-else class="px-3 py-2 font-mono text-xs leading-5 text-foreground/80 whitespace-pre-wrap break-words">{{ execOutputCache[group.block.id]?.content }}</pre>
                      </div>
                    </div>
                  </div>
                </div>
              </div>
              <div
                v-else-if="group.kind === 'single' && group.block.kind === 'notification'"
                class="mb-3 flex items-center gap-1 text-base text-muted-foreground"
                data-testid="timeline-notification-block"
                :data-block-id="group.block.id"
              >
                <component :is="icons.layers" class="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                <button
                  v-if="subagentTargetByNotificationBlockId[group.block.id]"
                  type="button"
                  class="inline-flex min-w-0 items-center gap-1 text-left hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
                  :aria-label="`${labels.open}: ${requiredNotificationText(group.block)}`"
                  data-testid="timeline-subagent-chat-open"
                  @click="emit('open-agent', subagentTargetByNotificationBlockId[group.block.id]!)"
                >
                  <span>{{ requiredNotificationText(group.block) }}</span>
                  <component :is="icons.chevron" class="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
                </button>
                <template v-else>
                  {{ requiredNotificationText(group.block) }}
                </template>
              </div>
              <p
                v-else-if="group.block.kind === 'status'"
                class="mb-3 whitespace-pre-wrap text-base leading-relaxed"
                :class="group.block.status === 'error' || group.block.statusType === 'error' ? 'text-activity-warning' : 'text-muted-foreground'"
                data-testid="timeline-status-block"
                :data-block-id="group.block.id"
              >
                <TimelineSearchHighlightedText
                  :text="statusBlockText(group.block)"
                  :matches="searchMatchesBySource[timelineBlockSearchSourceId(group.block.id)] ?? EMPTY_SEARCH_MATCHES"
                  :active-match-id="activeSearchMatchId"
                />
              </p>
              <div
                v-else-if="group.block.kind === 'generated-image'"
                class="mb-3"
                data-testid="timeline-generated-image-block"
                :data-block-id="group.block.id"
              >
                <component
                  :is="generatedImageCardComponent"
                  v-if="!mediaComponent && (group.block.generatedImages ?? []).length > 0"
                  :images="group.block.generatedImages ?? []"
                  :compact="isBrowserScreenshotImageBlock(group.block)"
                />
              </div>
              <div
                v-else-if="group.block.kind === 'plan-draft'"
                class="mb-3 overflow-hidden rounded-md border border-violet-500/30 bg-violet-500/[0.04] text-sm leading-relaxed text-foreground shadow-sm shadow-violet-950/10"
                data-testid="timeline-plan-draft-block"
                :data-block-id="group.block.id"
              >
                <div class="flex min-h-9 items-center justify-between gap-3 border-b border-violet-500/20 px-3 py-2">
                  <div class="inline-flex items-center gap-1.5 rounded bg-violet-500/10 px-2 py-1 text-xs font-semibold text-activity-violet">
                    <component :is="icons.listChecks" class="h-3.5 w-3.5" />
                    <span>Plan</span>
                  </div>
                  <span
                    v-if="planDraftMeta(group.block)"
                    class="min-w-0 truncate text-xs text-muted-foreground"
                    data-testid="timeline-plan-draft-meta"
                  >
                    {{ planDraftMeta(group.block) }}
                  </span>
                </div>
                <TimelineSearchDomText
                  :query="searchQuery"
                  :matches="searchMatchesBySource[timelineBlockSearchSourceId(group.block.id)] ?? EMPTY_SEARCH_MATCHES"
                  :active-match-id="activeSearchMatchId"
                >
                  <div class="px-4 py-3">
                    <component :is="markdownComponent ?? MarkdownRender"
                      :key="markdownRenderKey(group.block)"
                      v-bind="markdownRenderBindings(group.block)"
                      class="timeline-markdown timeline-markdown--assistant"
                      :content="group.block.content"
                      :streaming="group.block.status === 'running'"
                      :final="group.block.status !== 'running'"
                      :fade="group.block.status === 'running'"
                      :is-dark="isDark"
                      :code-block-light-theme="codeBlockLightTheme"
                      :code-block-dark-theme="codeBlockDarkTheme"
                      :code-block-props="codeBlockProps"
                      :code-block-options="codeBlockOptions"
                    />
                  </div>
                </TimelineSearchDomText>
              </div>
              <component
                :is="toolCallItemComponent"
                v-else
                :block="group.block"
                :expanded="blockExpandedById[group.block.id] ?? false"
                :session-id="sessionId"
                @toggle="toggleBlock(group.block.id)"
                @preview-file="emit('preview-file', $event)"
              />
              <component
                :is="mcpAppInlineAnchorComponent"
                v-if="mcpAppInlineAnchorsEnabled && group.kind === 'single' && sessionId && isMcpAppToolBlock(group.block)"
                :session-id="sessionId"
                :block="group.block"
              />
            </TimelineDetailsRegion>
          </template>
        </div>

        <!-- Turn terminal line: run_terminal / user_interrupt rendered as the
             turn's terminal state. Toolcall-line structure: icon + label +
             reason + chevron, inline expand via collapse-grid. -->
        <div
          v-if="hasTurnTerminal"
          class="mb-3"
          data-testid="timeline-terminal-line"
        >
          <div class="flex items-center gap-2">
            <component
              :is="buttonComponent"
              variant="ghost"
              class="group/terminal !p-0 h-auto min-w-0 flex-1 justify-start gap-1 rounded-none text-base font-normal shadow-none transition-colors hover:bg-transparent dark:hover:bg-transparent"
              :class="{ 'cursor-default': !turnTerminalExpandable }"
              :aria-expanded="turnTerminalExpandable ? turnTerminalExpanded : undefined"
              data-testid="timeline-terminal-toggle"
              @click="turnTerminalExpandable && emit('toggle-block', `terminal:${props.terminal!.sourceMessageId}`)"
            >
              <component
                :is="turnTerminalIcon"
                class="h-3.5 w-3.5 shrink-0 transition-colors mr-0.5"
                :class="turnTerminalFailed ? 'text-activity-warning' : 'text-muted-foreground group-hover/terminal:text-foreground'"
                aria-hidden="true"
                data-testid="timeline-terminal-icon"
              />
              <span
                class="min-w-0 truncate transition-colors"
                :class="turnTerminalFailed ? 'text-activity-warning' : 'text-muted-foreground group-hover/terminal:text-foreground'"
                data-testid="timeline-terminal-label"
              >
                <TimelineSearchHighlightedText
                  :text="turnTerminalLineText"
                  :matches="turnTerminalSearchMatches"
                  :active-match-id="activeSearchMatchId"
                />
              </span>
              <component
                :is="icons.chevron"
                v-if="turnTerminalExpandable"
                class="h-4 w-4 shrink-0 text-muted-foreground transition-colors group-hover/terminal:text-foreground"
                :class="{ 'rotate-90': turnTerminalExpanded }"
              />
            </component>
            <component
              :is="buttonComponent"
              v-if="hasTurnTerminalPlanAction && !readOnly"
              variant="outline"
              size="sm"
              class="shrink-0"
              :disabled="turnTerminalPlanActionPending"
              data-testid="dim-oauth-relay-view-plan"
              @click="openTurnTerminalPlanAction"
            >
              <component :is="icons.externalLink" class="h-3.5 w-3.5 -translate-y-[0.04em]" />
              {{ labels.planRenew }}
            </component>
          </div>
          <div
            v-if="turnTerminalExpandable"
            class="collapse-grid"
            :class="{ 'collapse-grid-open': turnTerminalExpanded }"
          >
            <div class="collapse-grid-inner" :inert="!turnTerminalExpanded || undefined">
              <div class="border-l border-border/60 pt-1 pl-3 ml-1.5 space-y-2 overflow-hidden">
                <p class="whitespace-pre-wrap text-sm leading-relaxed text-muted-foreground">
                  <TimelineSearchHighlightedText
                    :text="turnTerminalExplainText"
                    :matches="turnTerminalSearchMatches"
                    :active-match-id="activeSearchMatchId"
                    :base-offset="turnTerminalLineText.length + 1"
                  />
                </p>
                <div
                  v-if="turnTerminalRuntimeHostText !== '' || turnTerminalDebugLines.length > 0"
                  class="max-w-full overflow-hidden rounded border border-border/60 bg-muted/30 px-3 py-2 text-xs text-muted-foreground"
                  data-testid="timeline-terminal-debug-details"
                >
                  <div class="mb-2 flex items-center justify-between gap-2">
                    <span class="font-medium text-foreground">
                      {{ labels.terminalErrorDetailsTitle }}
                    </span>
                    <button
                      type="button"
                      class="inline-flex shrink-0 items-center rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
                      data-testid="timeline-terminal-debug-copy"
                      :title="labels.copy"
                      :aria-label="labels.copy"
                      @click="copyTurnTerminalDebugDetails"
                    >
                      <component :is="turnTerminalDebugCopied ? icons.check : icons.copy" class="h-3.5 w-3.5" :class="{ 'text-green-600': turnTerminalDebugCopied }" />
                    </button>
                  </div>
                  <p
                    v-if="turnTerminalRuntimeHostText !== ''"
                    class="mb-2 break-words font-mono text-[11px] leading-relaxed text-muted-foreground"
                    data-testid="timeline-terminal-runtime-host"
                  >
                    {{ turnTerminalRuntimeHostText }}
                  </p>
                  <dl class="space-y-2">
                    <template
                      v-for="line in turnTerminalDebugLines"
                      :key="line.key"
                    >
                      <dt class="text-[11px] font-semibold text-foreground/70">
                        {{ line.label }}
                      </dt>
                      <dd class="mt-1">
                        <pre class="max-h-40 overflow-auto whitespace-pre-wrap break-words font-mono text-[11px] leading-relaxed">{{ line.value }}</pre>
                      </dd>
                    </template>
                  </dl>
                </div>
              </div>
            </div>
          </div>
        </div>

        <div v-if="model" class="mt-2 flex items-center gap-2 text-[11px] text-muted-foreground">
          <span>{{ model }}</span>
        </div>
      </div>
    </component>
    <component :is="contextMenuContentComponent" class="w-52">
      <component
        :is="contextMenuItemComponent"
        data-testid="timeline-assistant-copy-selection"
        :disabled="!hasSelectedText"
        @select="copySelectedText"
      >
        <component :is="icons.copy" class="mr-2 h-4 w-4" />
        <span>{{ labels.copyMessage }}</span>
      </component>
      <component
        :is="contextMenuItemComponent"
        data-testid="timeline-assistant-copy-entire-message"
        :disabled="!hasCopyPayload"
        @select="copyText(copyPayload)"
      >
        <component :is="icons.copy" class="mr-2 h-4 w-4" />
        <span>{{ labels.copyEntireMessage }}</span>
      </component>
    </component>
  </component>
  <component
    :is="selectionPopoverComponent"
    v-if="selectionEnabled && selectionPopoverOpen && selectionQuote"
    :anchor-point="selectionAnchor"
    source-label="annotation"
    :allow-modify="false"
    @add-reference="addMessageReference"
    @cancel="closeSelectionPopover"
  />
</template>

<style scoped>
.collapse-grid {
  display: grid;
  grid-template-rows: 0fr;
  transition: grid-template-rows 300ms cubic-bezier(0.16, 1, 0.3, 1);
}

.collapse-grid-open {
  grid-template-rows: 1fr;
}

.collapse-grid-inner {
  overflow: hidden;
  min-height: 0;
  opacity: 0;
  transition: opacity 300ms ease-in;
}

.collapse-grid-open .collapse-grid-inner {
  opacity: 1;
}
</style>
