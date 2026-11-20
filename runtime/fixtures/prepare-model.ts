import assert from 'node:assert/strict'
import { definePlugin } from '@arcships/rutis'
import type { PrepareModelHost, SessionMethods } from '../../npm/plugin.js'
import { partRow } from './part-row.ts'

const part = definePlugin({
  name: 'PrepareModelFixture',
  inject: ['prepareModelHost', 'sessionMethods'],
  async apply(ctx) {
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
      return { kind: 'value', value: { providerId: settings.providerId, modelId: settings.modelId } }
    })
  },
})
export default partRow('session', part)
