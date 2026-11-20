import { definePlugin } from '@arcships/rutis'
import type { ContextUsage, SessionMethods } from '../../npm/plugin.js'
import { partRow } from './part-row.ts'

const part = definePlugin({
  name: 'ContextUsageFixture',
  inject: ['contextUsage', 'sessionMethods'],
  async apply(ctx) {
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
        return { kind: 'value', value: { ...reply, through: Number(reply.through) } }
      } finally {
        release()
      }
    })
  },
})
export default partRow('branch', part)
