import { describe, expect, it } from 'vitest'
import {
  createToolCallRegistry,
  buildTimelineToolTrace,
  buildToolCallLabelKey,
  buildToolInputPreview,
  classifyTimelineToolCategory,
  getToolDisplayNameKey,
  getTimelineToolLabelKey,
  resolveInlineParameterText,
  ToolCallRendererRegistry,
  type ChatUiToolRenderBlock,
  type ToolCallRenderer,
} from '../src'

const block = (overrides: Partial<ChatUiToolRenderBlock> = {}): ChatUiToolRenderBlock => ({
  id: 'block-1',
  kind: 'tool',
  status: 'complete',
  content: '',
  toolName: 'read',
  toolParams: { path: '/repo/src/a.ts' },
  ...overrides,
})

const renderer = (id: string, match: ToolCallRenderer['match']): ToolCallRenderer => ({
  id,
  match,
  getLabelKey: () => 'toolDisplay.readFile',
  getPrimaryParameter: () => null,
  getDetailSections: () => [],
  hasDetails: () => false,
})

describe('ToolCallRendererRegistry', () => {
  it('keeps registration order and the first match wins', () => {
    const registry = new ToolCallRendererRegistry()
    registry.register(renderer('first', () => true))
    registry.register(renderer('second', () => true))

    expect(registry.find('read', null).id).toBe('first')
  })

  it('throws when no renderer matches instead of inventing a fallback', () => {
    const registry = new ToolCallRendererRegistry()
    registry.register(renderer('only-file', toolName => toolName === 'read'))

    expect(() => registry.find('exec', null)).toThrow(/No renderer found/)
  })

  it('throws for any tool name when no renderer is registered', () => {
    expect(() => new ToolCallRendererRegistry().find('read', null)).toThrow(/generic fallback/)
  })
})

describe('createToolCallRegistry precedence', () => {
  const registry = createToolCallRegistry()

  it('routes known tools to their dedicated renderer', () => {
    expect(registry.find('read', {}).id).toBe('file')
    expect(registry.find('exec', {}).id).toBe('command')
    expect(registry.find('grep', {}).id).toBe('search')
    expect(registry.find('web_search', {}).id).toBe('web')
    expect(registry.find('todowrite', {}).id).toBe('todos')
    expect(registry.find('ask_user', {}).id).toBe('ask-user')
    expect(registry.find('agent', {}).id).toBe('agent')
    expect(registry.find('wait', {}).id).toBe('wait')
    expect(registry.find('skill', {}).id).toBe('skill')
    expect(registry.find('browser', {}).id).toBe('browser')
    expect(registry.find('cua-driver__click', {}).id).toBe('cua')
  })

  it('routes an MCP-sourced call to the MCP renderer before the tool name renderer', () => {
    const mcpBlock = block({
      toolName: 'read',
      toolSource: { kind: 'mcp', mcpServerId: 'github', mcpToolName: 'read_file' },
    })

    expect(registry.find('read', { path: '/repo/a.ts' }, mcpBlock).id).toBe('mcp')
  })
})

describe('buildTimelineToolTrace', () => {
  it('collects the renderer id, label, primary parameter, and flattened detail lines', () => {
    const trace = buildTimelineToolTrace(block({
      toolName: 'exec',
      toolParams: { command: 'pnpm build' },
      toolResult: { output: 'building\nbuilt' },
    }))

    expect(trace).toMatchObject({
      rawToolName: 'exec',
      toolLabelKey: 'toolDisplay.runCommand',
      rendererId: 'command',
      rendererKind: 'known',
      primaryParameter: { kind: 'text', text: 'pnpm' },
    })
    expect(trace.detailSections.map(section => section.id)).toEqual(['command', 'preview'])
    expect(trace.detailLines).toEqual(['pnpm build', 'building', 'built'])
  })

  it('keeps an unknown third-party tool identifier visible as the parameter', () => {
    expect(buildTimelineToolTrace(block({ toolName: 'third_party_tool', toolParams: {} })).primaryParameter)
      .toEqual({ kind: 'text', text: 'third_party_tool' })
  })

  it('uses the MCP source identity as the dynamic label text', () => {
    const trace = buildTimelineToolTrace(block({
      toolName: 'read',
      toolSource: { kind: 'mcp', mcpServerId: 'github', mcpToolName: 'read_file' },
    }))

    expect(trace.rendererId).toBe('mcp')
    expect(trace.toolLabelKey).toBe('timelineToolSummary.usedTool')
    expect(trace.toolLabelText).toBe('MCP · github/read_file')
  })

  it('resolves i18n parameters through the host translate callback', () => {
    const trace = buildTimelineToolTrace(block({
      toolName: 'ask_user',
      toolParams: { questions: [{ id: 'q', header: 'Choice', question: 'Pick one' }] },
      toolResult: { structuredContent: { code: 'ask_user_timed_out', waitedMs: 300_000 } },
    }))

    expect(trace.primaryParameter).toEqual({
      kind: 'i18n',
      key: 'toolAskUser.timedOut',
      params: { duration: '5m 0s' },
    })
    expect(resolveInlineParameterText(trace.primaryParameter, (key, params) =>
      params && typeof params.duration === 'string' ? `${key}(${params.duration})` : key))
      .toBe('toolAskUser.timedOut(5m 0s)')
  })
})

describe('tool labels', () => {
  it('maps the captured upstream tool names to display labels', () => {
    for (const toolName of ['read', 'exec', 'exec_stop', 'goal_update', 'ask_user', 'wait', 'skill']) {
      expect(getToolDisplayNameKey(toolName), toolName).not.toBeNull()
    }
    expect(getToolDisplayNameKey('third_party_tool')).toBeNull()
  })

  it('uses agent-action-specific labels and falls back to the category key', () => {
    expect(getTimelineToolLabelKey('agent', 'subagent', { action: 'create' })).toBe('toolDisplay.createAgent')
    expect(getTimelineToolLabelKey('agent', 'subagent', { action: 'list' })).toBe('toolDisplay.listAgents')
    expect(getTimelineToolLabelKey('agent', 'subagent', { action: 'cancel' })).toBe('toolDisplay.cancelAgent')
    expect(getTimelineToolLabelKey('agent', 'subagent', { action: 'models' })).toBe('toolDisplay.agent')
    expect(getTimelineToolLabelKey(undefined, 'unknown')).toBe('timelineToolSummary.usedTool')
  })

  it('uses action-specific keys for browser and computer-use calls', () => {
    const browser = buildTimelineToolTrace(block({
      toolName: 'browser',
      toolParams: { action: 'navigate', url: 'https://example.com/docs' },
    }))
    expect(browser.primaryParameter).toEqual({
      kind: 'i18n',
      key: 'toolBrowser.navigate',
      params: { url: 'https://example.com/docs' },
    })

    const cua = buildTimelineToolTrace(block({
      toolName: 'cua-driver__click',
      toolParams: { element_index: 4 },
    }))
    expect(cua.primaryParameter).toEqual({
      kind: 'i18n',
      key: 'toolCua.click',
      params: { target: ' · #4' },
    })
  })

  it('routes the buildToolCallLabelKey helper through the registry', () => {
    expect(buildToolCallLabelKey('exec', { command: 'pnpm test', runInBackground: true }))
      .toBe('toolDisplay.runCommandBackground')
  })
})

describe('classifyTimelineToolCategory', () => {
  it('keeps plugin skill aliases and unknown tools distinct', () => {
    expect(classifyTimelineToolCategory('skill')).toBe('skill')
    expect(classifyTimelineToolCategory('plugin__skill')).toBe('skill')
    expect(classifyTimelineToolCategory('generate_image')).toBe('unknown')
    expect(classifyTimelineToolCategory('cua-driver__zoom')).toBe('computerUse')
  })
})

describe('buildToolInputPreview', () => {
  it('reads a raw command as a single preview line', () => {
    expect(buildToolInputPreview('exec', { command: 'pnpm build' })).toEqual(['pnpm build'])
  })

  it('falls back to a generic key/value preview', () => {
    expect(buildToolInputPreview('third_party_tool', { alpha: 1, beta: 'two' }))
      .toEqual(['alpha: 1', 'beta: two'])
  })
})
