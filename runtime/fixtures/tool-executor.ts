import assert from 'node:assert/strict'
import { definePlugin } from '@arcships/rutis'
import type { SessionMethods, ToolExecutor } from '../../npm/plugin.js'
import { partRow } from './part-row.ts'

const part = definePlugin({
  name: 'ToolExecutorFixture',
  inject: ['toolExecutor', 'sessionMethods'],
  async apply(ctx) {
    const executor = ctx.use<ToolExecutor>('toolExecutor')
    let cancelChild: (() => void) | undefined
    await ctx.use<SessionMethods>('sessionMethods').register('session.nestedfixture', [
      { name: 'session.nestedfixture.run', mutating: false, interruption: 'report', progress: false },
      { name: 'session.nestedfixture.cancel', mutating: false, interruption: 'report', progress: false },
    ], async call => {
      const { turnId, requestId, branchId, parentCallId } = call.params
      if (typeof turnId !== 'string' || typeof requestId !== 'string' ||
          typeof branchId !== 'string' || typeof parentCallId !== 'string') throw new Error('missing context')
      if (call.method === 'session.nestedfixture.cancel') {
        if (!cancelChild) throw new Error('no child is running')
        const gateId = executor.open({ parentCallId, context: { turnId, requestId, branchId } }, () => new Promise<void>(() => {}))
        await executor.invoke({ gateId, call: { id: 'nested-grandchild', name: 'echo', input: { text: 'grandchild' } } })
        assert.deepEqual(await executor.close({ gateId }), [{ kind: 'fileWritten', path: '/fixture' }])
        cancelChild()
        return { kind: 'value', value: {} }
      }
      let release!: () => void
      const cancelled = new Promise<void>(resolve => { release = resolve })
      assert.throws(() => executor.open({
        parentCallId: 'unknown-parent', context: { turnId, requestId, branchId },
      }, () => cancelled), /no current tool batch/)
      const gateId = executor.open({ parentCallId, context: { turnId, requestId, branchId } }, () => cancelled)
      const child = { id: 'nested-echo', name: 'echo', input: { text: 'from node' } }
      try {
        const result = await executor.invoke({ gateId, call: child })
        await assert.rejects(async () => executor.invoke({ gateId, call: { ...child, id: parentCallId } }), /already exists/)
        await assert.rejects(async () => executor.invoke({ gateId, call: child }), /already exists/)
        const effects = await executor.close({ gateId })
        await assert.rejects(async () => executor.invoke({ gateId, call: child }), /closed/)
        assert.ok(typeof result.content === 'string')
        assert.deepEqual(effects, [{ kind: 'fileWritten', path: '/fixture' }])
        const waitingGate = executor.open({ parentCallId, context: { turnId, requestId, branchId } }, () => cancelled)
        cancelChild = release
        const waiting = executor.invoke({ gateId: waitingGate, call: {
          id: 'nested-wait', name: 'wait', input: { millis: 60000 },
        } })
        const interrupted = await waiting
        assert.equal(interrupted.isError, true)
        assert.match(String(interrupted.content), /cancel/i)
        await assert.rejects(async () => executor.invoke({ gateId: waitingGate, call: child }), /cancel/i)
        assert.deepEqual(await executor.close({ gateId: waitingGate }), effects)
        return { kind: 'value', value: { output: result.content, effectCount: effects.length } }
      } finally {
        cancelChild = undefined
        release()
      }
    })
  },
})
export default partRow('branch', part)
