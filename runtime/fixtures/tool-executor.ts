import assert from 'node:assert/strict'
import { definePlugin } from '@arcships/rutis'
import type { SessionMethods, ToolExecutor } from '../../npm/plugin.js'
import { partRow } from './part-row.ts'

const part = definePlugin({
  name: 'ToolExecutorFixture',
  inject: ['toolExecutor', 'sessionMethods'],
  async apply(ctx) {
    const executor = ctx.use<ToolExecutor>('toolExecutor')
    await ctx.use<SessionMethods>('sessionMethods').register('session.nestedfixture', [
      { name: 'session.nestedfixture.run', mutating: false, interruption: 'report', progress: false },
    ], async call => {
      const { turnId, requestId, branchId, parentCallId } = call.params
      if (typeof turnId !== 'string' || typeof requestId !== 'string' ||
          typeof branchId !== 'string' || typeof parentCallId !== 'string') throw new Error('missing context')
      let release!: () => void
      const cancelled = new Promise<void>(resolve => { release = resolve })
      const gateId = executor.open({ parentCallId, context: { turnId, requestId, branchId } }, () => cancelled)
      const child = { id: 'nested-echo', name: 'echo', input: { text: 'from node' } }
      try {
        const result = await executor.invoke({ gateId, call: child })
        const effects = await executor.close({ gateId })
        await assert.rejects(async () => executor.invoke({ gateId, call: child }), /closed/)
        assert.ok(typeof result.content === 'string')
        assert.deepEqual(effects, [])
        const waitingGate = executor.open({ parentCallId, context: { turnId, requestId, branchId } }, () => cancelled)
        const waiting = executor.invoke({ gateId: waitingGate, call: {
          id: 'nested-wait', name: 'wait', input: { millis: 60000 },
        } })
        release()
        const interrupted = await waiting
        assert.equal(interrupted.isError, true)
        assert.match(String(interrupted.content), /cancel/i)
        assert.deepEqual(await executor.close({ gateId: waitingGate }), [])
        return { kind: 'value', value: { output: result.content, effectCount: effects.length } }
      } finally {
        release()
      }
    })
  },
})
export default partRow('branch', part)
