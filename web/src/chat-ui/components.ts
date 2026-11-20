import { computed, defineComponent, h, type Component, type PropType } from "vue"
import {
  EditReplacementList,
  TimelineAssistantActions,
  TimelineAssistantMessage,
  TimelineBlockGroup,
  TimelineErrorBoundary,
  TimelineRow,
  TimelineTextItem,
  TimelineThinkingItem,
  TimelineToolCallItem,
  TimelineUserMessageRail,
  ToolCallInlineSummary,
  ToolCodeBlock,
  classifyTimelineToolCategory,
  formatWorkDuration,
  useThinkingExcerpt,
  type ChatUiAssistantMessageBlock,
  type ChatUiAssistantMessageIcons,
  type ChatUiTimelineBlockGroupIcons,
  type ChatUiTimelineBlockGroupSummaryItem,
  type ChatUiTimelineToolCallIcons,
  type ChatUiTimelineToolSummaryCategory,
  type ChatUiToolCallInlineIcons,
  type ChatUiTurnTerminalState,
  type TimelineToolInlineParameter,
  type TimelineToolTrace,
} from "@dimcode/chat-ui"
import ChatMarkdown from "../features/chat/ChatMarkdown.vue"
import { failureDetails } from "../failures"
import { i18n } from "../i18n"
import { toolTranslate } from "./i18n"
import { Button, Empty, Passthrough, icon, icons, tooltip, useWebTimeline, type WebTimelineContext } from "./primitives"
import { ICON } from "./WebIcon.vue"
import WebUserMessage from "./WebUserMessage.vue"

const t = (key: string, params: Record<string, string | number> = {}, plural?: number): string =>
  plural === undefined ? i18n.global.t(key, params) : i18n.global.t(key, params, plural)

const ACTION_KEYS: Record<ChatUiTimelineToolSummaryCategory, string> = {
  read: "chatTimeline.action.read",
  edit: "chatTimeline.action.edit",
  command: "chatTimeline.action.command",
  search: "chatTimeline.action.search",
  webSearch: "chatTimeline.action.webSearch",
  webFetch: "chatTimeline.action.webFetch",
  subagent: "chatTimeline.action.subagent",
  browser: "chatTimeline.action.browser",
  computerUse: "chatTimeline.action.computerUse",
  wait: "chatTimeline.action.wait",
  skill: "chatTimeline.action.skill",
  unknown: "chatTimeline.action.unknown",
}

const COUNT_KEYS: Record<ChatUiTimelineBlockGroupSummaryItem["key"], string> = {
  thinking: "work.thinkingSteps",
  read: "work.read",
  edit: "work.edit",
  command: "work.command",
  search: "work.search",
  webSearch: "work.webSearch",
  webFetch: "work.webFetch",
  subagent: "chatTimeline.work.subagent",
  browser: "chatTimeline.work.browser",
  computerUse: "chatTimeline.work.computerUse",
  wait: "chatTimeline.work.wait",
  skill: "chatTimeline.work.skill",
  unknown: "work.unknown",
  notifications: "chatTimeline.work.notifications",
}

/** Every label the shared components take as props, in the current locale. */
export function createLabels() {
  const backgroundTaskStatus = {
    completed: t("chatUi.timeline.notice.backgroundTaskStatus.completed"),
    failed: t("chatUi.timeline.notice.backgroundTaskStatus.failed"),
    cancelled: t("chatUi.timeline.notice.backgroundTaskStatus.cancelled"),
  }
  const workAction = Object.fromEntries(
    Object.entries(ACTION_KEYS).map(([category, key]) => [category, t(key)]),
  ) as Record<ChatUiTimelineToolSummaryCategory, string>
  const runFailedPlaceholder = t("chatUi.timeline.runFailedPlaceholder")
  const userInterrupt = t("chatUi.timeline.notice.userInterrupt")
  return {
    rowFailed: t("chatTimeline.rowFailed"),
    stopped: t("timeline.noticeStopped"),
    assistant: {
      runFailedPlaceholder,
      userInterrupt,
      backgroundTaskCommand: t("chatUi.timeline.notice.backgroundTaskCommand"),
      backgroundTaskStatus,
      open: t("chatUi.timeline.open"),
      copyMessage: t("chatUi.timeline.copyMessage"),
      copyEntireMessage: t("chatUi.timeline.copyEntireMessage"),
      terminalErrorDetailsTitle: t("chatUi.timeline.notice.runTerminalErrorDetails.title"),
      copy: t("chatUi.timeline.copy"),
      planRenew: t("chatTimeline.planRenew"),
      workAction,
      workActionSeparator: t("work.actionSeparator"),
      workActionMoreSuffix: t("chatTimeline.moreSuffix"),
      workSummary: ({ terminalStatus, actionsText, durationText }: { terminalStatus: "error" | "cancelled" | null; actionsText: string; durationText: string | null }) => [
        terminalStatus === "error" ? t("chatUi.timeline.runningFailed") : terminalStatus === "cancelled" ? t("chatUi.timeline.workStopped") : "",
        actionsText === "" ? t("chatTimeline.workDetails") : actionsText,
        durationText ?? "",
      ].filter(part => part !== "").join(t("work.separator")),
    },
    actions: {
      copyEntireMessage: t("chatUi.timeline.copyEntireMessage"),
      forkFromHere: t("chatUi.timeline.forkFromHere"),
      retry: t("chatUi.timeline.retry"),
    },
    copy: { runFailedPlaceholder, userInterrupt, copySuccess: t("chatUi.timeline.copySuccess") },
    group: {
      backgroundTaskCommand: t("chatUi.timeline.notice.backgroundTaskCommand"),
      open: t("chatUi.timeline.open"),
      separator: t("work.separator"),
      workActionSeparator: t("work.actionSeparator"),
      backgroundTaskStatus,
    },
    tool: {
      expandMessage: t("chatTimeline.tool.expand"),
      collapse: t("chatTimeline.tool.collapse"),
      loading: t("common.loading"),
      retry: t("chatTimeline.tool.retry"),
      askUserTimedOut: (duration: string) => t("chatTimeline.tool.askUserTimedOut", { duration }),
      askUserDeclined: t("chatTimeline.tool.askUserDeclined"),
    },
    inline: { queued: t("chatTimeline.tool.queued"), listFiles: t("toolDisplay.listFiles") },
    edit: {
      replacement: (index: number, count: number) => t("chatTimeline.edit.replacement", { index, count }),
      replaceAll: t("chatTimeline.edit.replaceAll"),
    },
    rail: {
      emptyChangedPaths: t("chatTimeline.rail.noChanges"),
      timeline: (count: number) => t("chatTimeline.rail.label", { count }),
      jumpToMessage: (preview: string) => t("chatTimeline.rail.jump", { preview }),
      option: (index: number, count: number, preview: string) => t("chatTimeline.rail.option", { index, count, preview }),
    },
    user: {
      withdraw: t("chatTimeline.user.withdraw"),
      copy: t("message.copy"),
      edit: t("chatTimeline.user.edit"),
      fork: t("chatUi.timeline.forkFromHere"),
      branch: t("message.branch"),
    },
    summaryItem: (item: ChatUiTimelineBlockGroupSummaryItem) => t(COUNT_KEYS[item.key], { count: item.count }, item.count),
  }
}

export type WebTimelineLabels = ReturnType<typeof createLabels>

const assistantIcons: ChatUiAssistantMessageIcons = {
  chevron: icons.chevronRight,
  copy: icons.copy,
  check: icons.check,
  externalLink: icon(ICON.externalLink),
  listChecks: icon(ICON.listChecks),
  bot: icon(ICON.bot),
  terminal: icon(ICON.terminal),
  layers: icon(ICON.layers),
  triangleAlert: icon(ICON.triangleAlert),
  octagonPause: icon(ICON.octagonPause),
}

const groupIcons: ChatUiTimelineBlockGroupIcons = {
  workflow: icon(ICON.workflow),
  chevron: icons.chevronRight,
  bot: assistantIcons.bot,
  terminal: assistantIcons.terminal,
  layers: assistantIcons.layers,
}

const toolIcons: ChatUiTimelineToolCallIcons = {
  chevron: icons.chevronRight,
  todoCompleted: icon(ICON.circleCheck),
  todoInProgress: icon(ICON.loaderCircle),
  todoPending: icon(ICON.circle),
}

const inlineIcons: ChatUiToolCallInlineIcons = Object.fromEntries(Object.entries({
  read: ICON.fileText,
  search: ICON.search,
  files: ICON.folder,
  "file-search": ICON.fileSearch,
  "file-write": ICON.filePlus,
  "file-edit": ICON.filePen,
  terminal: ICON.terminal,
  web: ICON.globe,
  browser: ICON.appWindow,
  cua: ICON.mousePointerClick,
  "ask-user": ICON.messageCircleQuestion,
  todos: ICON.listTodo,
  skill: ICON.sparkles,
  wait: ICON.hourglass,
  agent: ICON.bot,
  mcp: ICON.plug,
  generic: ICON.wrench,
}).map(([key, name]) => [key, { component: icon(name) }]))

const AnimatedNumber = defineComponent({
  name: "WebChatNumber",
  props: { value: { type: Number, required: true }, prefix: { type: String, default: "" }, durationMs: Number },
  setup: props => () => h("span", `${props.prefix}${props.value}`),
})

const codeTheme = { dark: "vitesse-dark", light: "vitesse-light" }
const CODE_FONT_SIZE = 13

const WebMarkdown = defineComponent({
  name: "WebChatMarkdown",
  inheritAttrs: false,
  setup: (_, { attrs }) => () => h(ChatMarkdown as Component, { ...attrs, messageId: String(attrs.indexKey ?? "") }),
})

const WebToolCodeBlock = defineComponent({
  name: "WebChatToolCodeBlock",
  inheritAttrs: false,
  setup(_, { attrs }) {
    const context = useWebTimeline()
    return () => h(ToolCodeBlock as Component, {
      ...attrs,
      theme: codeTheme,
      themeType: context.isDark.value ? "dark" : "light",
      codeFontSize: CODE_FONT_SIZE,
    })
  },
})

const WebEditReplacementList = defineComponent({
  name: "WebChatEditReplacementList",
  inheritAttrs: false,
  setup(_, { attrs }) {
    const context = useWebTimeline()
    return () => h(EditReplacementList as Component, { ...attrs, codeBlockComponent: WebToolCodeBlock, labels: context.labels.value.edit })
  },
})

function parameterText(parameter: TimelineToolInlineParameter | null): string {
  if (parameter === null) return ""
  return parameter.kind === "text" ? parameter.text : toolTranslate(parameter.key, parameter.params)
}

const WebInlineSummary = defineComponent({
  name: "WebChatInlineSummary",
  inheritAttrs: false,
  props: { trace: { type: Object as PropType<TimelineToolTrace>, required: true } },
  setup(props, { attrs }) {
    const context = useWebTimeline()
    return () => h(ToolCallInlineSummary as Component, {
      ...attrs,
      trace: {
        rawToolName: props.trace.rawToolName,
        rendererId: props.trace.rendererId,
        labelText: props.trace.toolLabelText ?? toolTranslate(props.trace.toolLabelKey),
        parameterText: parameterText(props.trace.primaryParameter),
        parameterTitle: props.trace.primaryParameter?.title ?? null,
      },
      labels: context.labels.value.inline,
      icons: inlineIcons,
      ...tooltip,
      providerIconComponent: Empty,
      faviconComponent: Empty,
    })
  },
})

const WebToolCallItem = defineComponent({
  name: "WebChatToolCallItem",
  inheritAttrs: false,
  setup(_, { attrs }) {
    const context = useWebTimeline()
    return () => h(TimelineToolCallItem as Component, {
      ...attrs,
      labels: context.labels.value.tool,
      icons: toolIcons,
      buttonComponent: Button,
      inlineSummaryComponent: WebInlineSummary,
      mediaPanelComponent: Empty,
      editReplacementListComponent: WebEditReplacementList,
      toolCodeBlockComponent: WebToolCodeBlock,
      faviconComponent: Empty,
      providerIconComponent: Empty,
      animatedNumberComponent: AnimatedNumber,
      ...tooltip,
    })
  },
})

const WebThinkingItem = defineComponent({
  name: "WebChatThinkingItem",
  inheritAttrs: false,
  props: {
    block: { type: Object as PropType<ChatUiAssistantMessageBlock>, required: true },
    expanded: { type: Boolean, required: true },
  },
  emits: ["toggle"],
  setup(props, { emit }) {
    const running = computed(() => props.block.status === "running")
    const excerpt = useThinkingExcerpt(() => props.block.content, () => running.value)
    const title = computed(() => !running.value && props.block.durationMs != null
      ? t("work.thoughtFor", { duration: formatWorkDuration(props.block.durationMs) })
      : t("work.thinking"))
    return () => h(TimelineThinkingItem, {
      blockId: props.block.id,
      content: props.block.content,
      isRunning: running.value,
      expanded: props.expanded,
      titleText: title.value,
      excerpt: excerpt.text.value,
      excerptTruncated: excerpt.truncated.value,
      onToggle: () => emit("toggle"),
    })
  },
})

const WebTextItem = defineComponent({
  name: "WebChatTextItem",
  inheritAttrs: false,
  setup(_, { attrs }) {
    const context = useWebTimeline()
    return () => h(TimelineTextItem as Component, {
      ...attrs,
      markdownComponent: WebMarkdown,
      isDark: context.isDark.value,
      codeBlockLightTheme: codeTheme.light,
      codeBlockDarkTheme: codeTheme.dark,
      codeBlockProps: {},
      codeBlockOptions: { fontSize: CODE_FONT_SIZE, lineHeight: 20 },
    })
  },
})

function toolCategories(blocks: readonly ChatUiAssistantMessageBlock[]): Record<string, ChatUiTimelineToolSummaryCategory> {
  const categories: Record<string, ChatUiTimelineToolSummaryCategory> = {}
  for (const block of blocks) if (block.kind === "tool") categories[block.id] = classifyTimelineToolCategory(block.toolName)
  return categories
}

const WebBlockGroup = defineComponent({
  name: "WebChatBlockGroup",
  inheritAttrs: false,
  props: { blocks: { type: Array as PropType<readonly ChatUiAssistantMessageBlock[]>, required: true } },
  setup(props, { attrs }) {
    const context = useWebTimeline()
    const categories = computed(() => toolCategories(props.blocks))
    return () => h(TimelineBlockGroup as Component, {
      ...attrs,
      blocks: props.blocks,
      toolCategoryByBlockId: categories.value,
      transitionEnabled: true,
      labels: context.labels.value.group,
      icons: groupIcons,
      buttonComponent: Button,
      ...tooltip,
      thinkingItemComponent: WebThinkingItem,
      toolCallItemComponent: WebToolCallItem,
      mcpAppInlineAnchorComponent: Empty,
      providerIconComponent: Empty,
      readBackgroundTaskNotice: () => null,
      getAgentModelLabel: () => null,
    }, { "summary-item": ({ item }: { item: ChatUiTimelineBlockGroupSummaryItem }) => context.labels.value.summaryItem(item) })
  },
})

function terminalExplain(context: WebTimelineContext, terminal: ChatUiTurnTerminalState | null | undefined): string {
  if (terminal == null) return ""
  const row = context.rowByKey(terminal.sourceMessageId)
  return row?.kind === "error" && row.details !== undefined ? failureDetails(row.details).join("\n") : ""
}

const WebAssistantMessage = defineComponent({
  name: "WebChatAssistantMessage",
  inheritAttrs: false,
  props: {
    blocks: { type: Array as PropType<readonly ChatUiAssistantMessageBlock[]>, default: () => [] },
    terminal: { type: Object as PropType<ChatUiTurnTerminalState | null>, default: null },
  },
  setup(props, { attrs }) {
    const context = useWebTimeline()
    const categories = computed(() => toolCategories(props.blocks))
    return () => {
      const labels = context.labels.value
      const terminal = props.terminal
      return h(TimelineAssistantMessage as Component, {
        ...attrs,
        blocks: props.blocks,
        terminal,
        terminalLineText: terminal === null ? "" : terminal.status === "stopped" ? labels.stopped : terminal.message ?? "",
        terminalExplainText: terminalExplain(context, terminal),
        toolCategoryByBlockId: categories.value,
        labels: labels.assistant,
        icons: assistantIcons,
        buttonComponent: Button,
        ...tooltip,
        contextMenuComponent: Passthrough,
        contextMenuTriggerComponent: Passthrough,
        contextMenuContentComponent: Empty,
        contextMenuItemComponent: Empty,
        blockGroupComponent: WebBlockGroup,
        thinkingItemComponent: WebThinkingItem,
        textItemComponent: WebTextItem,
        retryNoticeComponent: Empty,
        toolCallItemComponent: WebToolCallItem,
        mcpAppInlineAnchorComponent: Empty,
        generatedImageCardComponent: Empty,
        selectionPopoverComponent: Empty,
        providerIconComponent: Empty,
        markdownComponent: WebMarkdown,
        isDark: context.isDark.value,
        codeBlockLightTheme: codeTheme.light,
        codeBlockDarkTheme: codeTheme.dark,
        codeBlockProps: {},
        codeBlockOptions: { fontSize: CODE_FONT_SIZE, lineHeight: 20 },
        readBackgroundTaskNotice: () => null,
        getAgentModelLabel: () => null,
      })
    }
  },
})

const WebAssistantActions = defineComponent({
  name: "WebChatAssistantActions",
  inheritAttrs: false,
  setup(_, { attrs }) {
    const context = useWebTimeline()
    return () => h(TimelineAssistantActions as Component, {
      ...attrs,
      labels: context.labels.value.actions,
      copyLabels: context.labels.value.copy,
      locale: i18n.global.locale.value,
      icons: { check: icons.check, copy: icons.copy, fork: icons.gitFork, retry: icons.retry },
      buttonComponent: Button,
      tooltipProviderComponent: Passthrough,
      ...tooltip,
    })
  },
})

/** The row component the surface dispatches every row to. */
export const WebTimelineRow = defineComponent({
  name: "WebChatTimelineRow",
  inheritAttrs: false,
  setup: (_, { attrs }) => () => h(TimelineRow as Component, {
    ...attrs,
    userMessageComponent: WebUserMessage,
    assistantMessageComponent: WebAssistantMessage,
    artifactsComponent: Empty,
    fileChangesComponent: Empty,
    assistantActionsComponent: WebAssistantActions,
    compressionDividerComponent: Empty,
  }),
})

export const WebErrorBoundary = defineComponent({
  name: "WebChatErrorBoundary",
  setup(_, { slots }) {
    const context = useWebTimeline()
    return () => h(TimelineErrorBoundary, { fallbackText: context.labels.value.rowFailed }, slots)
  },
})

export const WebUserMessageRail = defineComponent({
  name: "WebChatUserMessageRail",
  inheritAttrs: false,
  setup(_, { attrs }) {
    const context = useWebTimeline()
    return () => h(TimelineUserMessageRail as Component, { ...attrs, labels: context.labels.value.rail })
  },
})
