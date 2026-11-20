import assert from 'node:assert/strict'
import { definePlugin } from '@arcships/rutis'
import type { ContextModelHost, ContextUsage, Message, SessionMethods } from '../../npm/plugin.js'
import { partRow } from './part-row.ts'

const part = definePlugin({
  name: 'ContextUsageFixture',
  inject: ['contextModelHost', 'contextUsage', 'sessionMethods'],
  async apply(ctx) {
    const counter = ctx.use<ContextModelHost>('contextModelHost')
    const usage = ctx.use<ContextUsage>('contextUsage')
    await ctx.use<SessionMethods>('sessionMethods').register('session.usagefixture', [
      { name: 'session.usagefixture.read', mutating: false, interruption: 'report', progress: false },
    ], async call => {
      const through = call.params.through
      if (typeof through !== 'number') throw new Error('missing cutoff')
      let release!: () => void
      const cancelled = new Promise<void>(done => { release = done })
      try {
        const reply = await usage.read({ through }, () => cancelled)
        const model = { providerId: 'mock', modelId: 'echo' }
        const messages: Message[] = [
          { role: 'user', content: 'some content' }, { role: 'assistant', content: 'a response' },
        ]
        const facts = await counter.describe(model)
        assert.equal(facts.contextWindow, reply.contextWindow)
        assert.equal(facts.outputTokens, reply.outputTokens)
        assert.equal(facts.thresholdTokens, reply.thresholdTokens)
        const parts = await counter.contributions({ model, fixed: { system: 'instructions' }, messages })
        assert.equal(parts.algorithm, facts.algorithm)
        assert.ok(parts.fixed)
        assert.equal(parts.messages.length, messages.length)
        const total = [parts.fixed, ...parts.messages].reduce<{ serialized_bytes: bigint; images: bigint }>((sum, item) => ({
          serialized_bytes: sum.serialized_bytes + BigInt(item.serialized_bytes),
          images: sum.images + BigInt(item.images),
        }), { serialized_bytes: 0n, images: 0n })
        const estimate = await counter.estimate({ model, algorithm: parts.algorithm, contribution: total })
        assert.equal(estimate, await counter.count({ ...model, system: 'instructions', messages }))
        await assert.rejects(counter.estimate({ model, algorithm: 'different', contribution: total }), /algorithm/)
        return { kind: 'value', value: { ...reply, through: Number(reply.through) } }
      } finally {
        release()
      }
    })
  },
})
export default partRow('branch', part)
