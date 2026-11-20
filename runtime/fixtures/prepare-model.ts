import assert from 'node:assert/strict'
import { definePlugin } from '@arcships/rutis'
import type { PrepareContextHost, PrepareModelHost, SessionMethods } from '../../npm/plugin.js'
import { partRow } from './part-row.ts'

const part = definePlugin({
  name: 'PrepareModelFixture',
  inject: ['prepareContextHost', 'prepareModelHost', 'sessionMethods'],
  async apply(ctx) {
    const context = ctx.use<PrepareContextHost>('prepareContextHost')
    const host = ctx.use<PrepareModelHost>('prepareModelHost')
    await ctx.use<SessionMethods>('sessionMethods').register('session.preparefixture', [
      { name: 'session.preparefixture.read', mutating: false, interruption: 'report', progress: false },
    ], async () => {
      const availableTools = ['zeta', 'alpha'].map(name => ({
        name, description: name, inputSchema: {}, source: 'extension' as const,
      }))
      const settings = host.prepare({
        turnId: 'turn', requestId: 'request',
        history: [{ role: 'user', content: 'prepare this request' }],
        availableTools,
      })
      assert.deepEqual(settings.tools, availableTools)
      const request = { ...settings, messages: [{ role: 'user' as const, content: 'original message' }] }
      const source = { turnId: 'turn', requestId: 'request', through: 9007199254740993n }
      let release!: () => void
      const cancelled = new Promise<void>(done => { release = done })
      try {
        assert.deepEqual(await context.prepare({ request, source }, () => cancelled), request)
        const waiting = context.prepare({ request, source: { ...source, requestId: 'waiting' } }, () => cancelled)
        release()
        await assert.rejects(waiting, /preparation stopped/)
      } finally {
        release()
      }
      return { kind: 'value', value: { providerId: settings.providerId, modelId: settings.modelId } }
    })
  },
})
export default partRow('session', part)
