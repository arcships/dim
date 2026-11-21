import assert from 'node:assert/strict'
import { definePlugin } from '@arcships/rutis'
import type { FileReads, SessionMethods, Services } from '../../npm/plugin.js'
import { partRow } from './part-row.ts'

const part = definePlugin({
  name: 'FileReadsFixture',
  inject: ['fileReads', 'timeline', 'sessionMethods'],
  async apply(ctx) {
    const reads = ctx.use<FileReads>('fileReads')
    const timeline = ctx.use<Services['timeline']>('timeline')
    await ctx.use<SessionMethods>('sessionMethods').register('session.filebinding', [
      { name: 'session.filebinding.copy', mutating: true, interruption: 'report', progress: false },
    ], async call => {
      const { source, target } = call.params
      if (typeof source !== 'string' || typeof target !== 'string') throw new Error('missing path')
      const established = await timeline.committed()
      const binding = reads.binding({ path: source })
      assert.ok(binding)
      assert.deepEqual(binding.encoding, { kind: 'legacy', name: 'cp932' })
      assert.deepEqual(await reads.writeBinding({ path: target }), { kind: 'absent' })
      reads.record({ path: target, binding, established })
      assert.deepEqual(await reads.writeBinding({ path: target }), { kind: 'bound', binding })
      return { kind: 'value', value: {} }
    })
  },
})
export default partRow('branch', part)
