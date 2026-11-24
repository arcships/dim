import { describe, expect, it } from 'vitest'
import {
  groupConsecutiveBlocks,
  isEmptyTimelineThinking,
  isGroupableWorkBlock,
  scanTimelineWorkGroupRuns,
  type ChatUiTimelineWorkBlockShape,
} from '../src'

type Block = ChatUiTimelineWorkBlockShape & { content?: string }

function block(id: string, kind: Block['kind'], extra: Partial<Block> = {}): Block {
  return { id, kind, ...extra }
}

function shape(value: Block): ChatUiTimelineWorkBlockShape {
  return {
    id: value.id,
    kind: value.kind,
    toolSourceKind: value.toolSourceKind,
    displayMode: value.displayMode,
    isEmptyThinking: value.isEmptyThinking,
  }
}

function group(blocks: Block[], minWorkBlocks?: number) {
  return groupConsecutiveBlocks(blocks, { minWorkBlocks, shape })
}

describe('timeline block grouping', () => {
  it('keeps a lone work block single', () => {
    const grouped = group([block('think-1', 'thinking'), block('text-1', 'text')])
    expect(grouped.map(item => item.kind)).toEqual(['single', 'single'])
  })

  it('honors a higher minimum work count', () => {
    const blocks = [block('think-1', 'thinking'), block('tool-1', 'tool')]
    expect(group(blocks, 3).map(item => item.kind)).toEqual(['single', 'single'])
  })

  it('treats empty running thinking as transparent but keeps it in the slice', () => {
    expect(isEmptyTimelineThinking({ kind: 'thinking', status: 'running', content: '  ' })).toBe(true)
    expect(isEmptyTimelineThinking({ kind: 'thinking', status: 'complete', content: '' })).toBe(false)
    expect(isEmptyTimelineThinking({ kind: 'tool', status: 'running', content: '' })).toBe(false)

    const blocks = [
      block('think-1', 'thinking'),
      block('empty-1', 'thinking', { isEmptyThinking: true }),
      block('tool-1', 'tool'),
    ]
    const grouped = group(blocks)
    expect(grouped).toHaveLength(1)
    const workGroup = grouped[0]
    if (workGroup?.kind === 'group') {
      expect(workGroup.blocks.map(value => value.id)).toEqual(['think-1', 'empty-1', 'tool-1'])
    }
  })

  it('splits inline MCP apps but groups fullscreen MCP tools', () => {
    expect(isGroupableWorkBlock(block('inline', 'tool', { toolSourceKind: 'mcp', displayMode: 'inline' }))).toBe(false)
    expect(isGroupableWorkBlock(block('full', 'tool', { toolSourceKind: 'mcp' }))).toBe(true)
    expect(isGroupableWorkBlock(block('full-inline-mode', 'tool', { toolSourceKind: 'mcp', displayMode: 'fullscreen' }))).toBe(true)

    const inline = group([
      block('think-1', 'thinking'),
      block('inline', 'tool', { toolSourceKind: 'mcp', displayMode: 'inline' }),
      block('tool-1', 'tool'),
    ])
    expect(inline.map(item => item.kind)).toEqual(['single', 'single', 'single'])
  })

  it('returns runs with start/end indexes and ids', () => {
    const blocks = [
      block('a', 'thinking'),
      block('b', 'tool'),
      block('c', 'text'),
      block('d', 'tool'),
      block('e', 'tool'),
    ]
    expect(scanTimelineWorkGroupRuns(blocks.length, index => blocks[index]!)).toEqual([
      { groupId: 'group:a', start: 0, end: 2 },
      { groupId: 'group:d', start: 3, end: 5 },
    ])
  })
})
