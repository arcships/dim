import { definePlugin } from '@arcships/rutis'
import { partRow } from './part-row.ts'
import provides from './tree-create.json' with { type: 'json' }
import assert from 'node:assert/strict'
import type { SessionMethodReply, SessionMethods, TimelineTree } from '../../npm/plugin.js'

export const part = definePlugin({
  name: 'TreeCreateFixture',
  provides,
  inject: ['timelineTree', 'sessionMethods'],
  async apply(ctx) {
    const tree = ctx.use<TimelineTree>('timelineTree')
    await ctx.use<SessionMethods>('sessionMethods').register(
      'session.treeprobe',
      [{ name: 'session.treeprobe.create', mutating: true, interruption: 'report', progress: false }],
      async ({ params }): Promise<SessionMethodReply> => {
        const { at, cancel } = params
        assert.ok(typeof at === 'number' || typeof at === 'bigint')
        assert.equal(typeof cancel, 'boolean')
        const options = { prompt: 'probe child', source: { kind: 'host' as const, operation_id: crypto.randomUUID() } }
        const cancelled = () => (cancel ? Promise.resolve() : new Promise<void>(() => {}))
        try {
          const id = await tree.create('main', at, options, cancelled)
          assert.deepEqual(await tree.call({ branchId: id, operation: { kind: 'approval.pending' } }, cancelled), [])
          assert.deepEqual(await tree.call({
            branchId: id, operation: { kind: 'inputQueue.withdraw', command: { input_id: 'absent' } },
          }, cancelled), { status: 'notHeld' })
          await assert.rejects(tree.call({
            branchId: id, operation: { kind: 'approval.decide', requestId: 'absent', decision: 'deny' },
          }, cancelled), { code: 'Unknown' })
          await assert.rejects(
            tree.call({ branchId: 'missing', operation: { kind: 'approval.pending' } }, cancelled),
            { code: 'session_invalid_input' },
          )
          const entry = await tree.directory({id})
          assert.ok(!Array.isArray(entry) && entry.info.id === id && entry.state === 'open')
          const directory = await tree.directory({})
          assert.ok(Array.isArray(directory) && directory.some(item => item.info.id === id))
          const detail = await tree.display(id)
          assert.equal(detail.entry.info.id, id)
          assert.equal(detail.purpose, 'probe child')
          await tree.batchEnd('main', at)
          assert.equal(Number(await tree.pageEnd('main', at, 1, at)), Number(at))
          return { kind: 'value', value: { id } }
        } catch (error) {
          return { kind: 'value', value: { error: (error as { code?: string }).code ?? String(error) } }
        }
      },
    )
  },
})

export default partRow('session', part)
