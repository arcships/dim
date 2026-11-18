import { definePlugin } from '@arcships/rutis'
import { partRow } from './part-row.ts'
import type { PromptRegistry, PromptEntry } from '../../npm/plugin.js'
import type { SessionIdentity } from '../../npm/branch-services.js'

const part = definePlugin<{ gate: string }>({
  name: 'PromptSourceFixture',
  provides: {},
  inject: ['promptRegistry', 'sessionIdentity'],
  async apply(ctx, config) {
    const identity = await ctx.use<SessionIdentity>('sessionIdentity').get()
    await ctx.use<PromptRegistry>('promptRegistry').register('fixture', async (target, cancelled) => {
      if (target.scope.kind === 'turn') {
        const id = target.scope.id
        let finished = false
        void cancelled().then(async () => { if (!finished) await fetch(`${config.gate}/cancel/${id}`) })
        await fetch(`${config.gate}/load/${id}`)
        finished = true
        if (id === 'abandoned') await fetch(`${config.gate}/settled/${id}`)
        if (id === 'cancel-error') throw Object.assign(new Error('source cancelled'), { code: 'cancelled' })
      }
      const entry: PromptEntry = {
        source: target.source, scope: target.scope, version: '1',
        body: identity.sessionId, delivery: 'context', order: 0,
      }
      return entry
    })
  },
})

export default partRow('session', part)
