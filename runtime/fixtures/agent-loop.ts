import { definePlugin } from '@arcships/rutis'
import type { AgentLoop, LoopSnapshot, SessionMethods } from '../../npm/plugin.js'
import { partRow } from './part-row.ts'

const part = definePlugin({
  name: 'AgentLoopFixture',
  inject: ['agentLoop', 'sessionMethods'],
  async apply(ctx) {
    const loop = ctx.use<AgentLoop>('agentLoop')
    let resolve!: (value: LoopSnapshot) => void
    const initial = new Promise<LoopSnapshot>(ready => { resolve = ready })
    const observation = loop.observe(state => resolve(state))
    await ctx.use<SessionMethods>('sessionMethods').register('session.loopfixture', [
      { name: 'session.loopfixture.compact', mutating: true, interruption: 'report', progress: false },
    ], async () => {
      const observed = await Promise.race([initial, observation.then(() => { throw new Error('observation ended') })])
      const state = await loop.state({})
      const stale = await loop.stopTurn({ turnId: 'absent', reason: 'fixture' })
      const sequence = await loop.compact({})
      return { kind: 'value', value: { observed, state, stale, sequence: Number(sequence) } }
    })
  },
})
export default partRow('branch', part)
