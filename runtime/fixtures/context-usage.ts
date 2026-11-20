import assert from 'node:assert/strict'
import { definePlugin } from '@arcships/rutis'
import type { CompactionStrategyHost, HistoryRenderer, ContextModelHost, ContextPreparation, ContextUsage, Message, SessionMethods } from '../../npm/plugin.js'
import { partRow } from './part-row.ts'

const part = definePlugin({
  name: 'ContextUsageFixture',
  inject: ['historyRenderer', 'compactionStrategyHost', 'contextModelHost', 'contextPreparation', 'contextUsage', 'sessionMethods'],
  async apply(ctx) {
    const renderer = ctx.use<HistoryRenderer>('historyRenderer')
    const strategy = ctx.use<CompactionStrategyHost>('compactionStrategyHost')
    const counter = ctx.use<ContextModelHost>('contextModelHost')
    const preparation = ctx.use<ContextPreparation>('contextPreparation')
    const usage = ctx.use<ContextUsage>('contextUsage')
    await ctx.use<SessionMethods>('sessionMethods').register('session.usagefixture', [
      { name: 'session.usagefixture.read', mutating: false, interruption: 'report', progress: false },
    ], async call => {
      const through = call.params.through
      if (typeof through !== 'number') throw new Error('missing cutoff')
      let release!: () => void
      const cancelled = new Promise<void>(done => { release = done })
      try {
        const prefix = { branchId: 'main', through }
        const history = await renderer.context(prefix)
        const rendered = await renderer.renderedContext(prefix)
        assert.deepEqual(rendered.context, history)
        assert.ok(history.positions.every(position => Number(position.sequence) <= through))
        assert.equal(history.positions.length, history.events.length)
        const reply = await usage.read({ through }, () => cancelled)
        const model = { providerId: 'mock', modelId: 'echo' }
        const messages: Message[] = [
          { role: 'user', content: 'some content' }, { role: 'assistant', content: 'a response' },
        ]
        const source = {
          turn_id: 'fixture-turn', request_id: 'fixture-request', through,
          request: { ...model, messages: [{ role: 'user' as const, content: 'prepare this request' }] },
        }
        const prepared = await preparation.prepare({ source }, () => cancelled)
        assert.equal(prepared.providerId, model.providerId)
        assert.equal(prepared.modelId, model.modelId)
        await preparation.validate({ requestId: source.request_id, request: prepared }, () => cancelled)
        assert.deepEqual(await strategy.describe(model), { name: 'fixture', replacementSize: 0 })
        await assert.rejects(strategy.compact({
          history: [], cuts: [], request: source.request, trigger: 'manual',
          estimatedInputTokens: 0, baselineSource: 'full_estimate',
        }, () => cancelled), /the CodeMode fixture fits without compaction/)
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
