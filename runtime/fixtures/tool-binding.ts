import { definePlugin } from '@arcships/rutis'
import type { SessionMethods, ToolBinding } from '../../npm/plugin.js'
import { partRow } from './part-row.ts'

const part = definePlugin({
  name: 'ToolBindingFixture',
  inject: ['toolBinding', 'sessionMethods'],
  async apply(ctx) {
    const binding = ctx.use<ToolBinding>('toolBinding')
    await ctx.use<SessionMethods>('sessionMethods').register('session.bindingfixture', [
      { name: 'session.bindingfixture.pin', mutating: true, interruption: 'report', progress: false },
    ], async () => {
      const declared = binding.bindCurrent({ requestId: 'fixture-current' })
      binding.bind({ requestId: 'fixture-empty', through: 0n, tools: [], toolSources: [] })
      let collision = ''
      try {
        binding.transfer({ from: 'fixture-current', to: 'fixture-empty' })
      } catch (error) {
        collision = String(error)
      }
      const empty = binding.definitions({ requestId: 'fixture-empty' })
      binding.release({ requestId: 'fixture-empty' })
      binding.transfer({ from: 'fixture-current', to: 'fixture-transferred' })
      const transferred = binding.definitions({ requestId: 'fixture-transferred' })
      binding.release({ requestId: 'fixture-transferred' })
      for (const requestId of ['fixture-current', 'fixture-empty', 'fixture-transferred']) {
        binding.bindCurrent({ requestId, declared: [] })
        binding.release({ requestId })
      }
      return { kind: 'value', value: {
        current: declared.map(tool => tool.name).sort(),
        transferred: transferred.map(tool => tool.name),
        echo: transferred.find(tool => tool.name === 'echo')?.description ?? null,
        empty: empty.length, collision,
      } }
    })
  },
})
export default partRow('branch', part)
