import { definePlugin } from '@arcships/rutis'
import type { ManagementMethods, HooksService } from '../../npm/plugin.js'

export default definePlugin({
  name: 'HooksServiceFixture',
  provides: {},
  inject: ['managementMethods', 'hooksService'],
  async apply(ctx) {
    const hooks = ctx.use<HooksService>('hooksService')
    await ctx.use<ManagementMethods>('managementMethods').register(
      'hooksprobe',
      ['register', 'read', 'withdraw'].map(action => ({
        name: `hooksprobe.${action}`, mutating: action !== 'read', interruption: 'report', progress: false,
      })),
      async call => {
        if (call.method === 'hooksprobe.register') {
          const params = call.params
          if (params === null || typeof params !== 'object' || Array.isArray(params) || typeof params.dir !== 'string' || params.document === undefined) throw new Error('expected dir and document')
          const reply = await hooks.register({ plugin: 'foreign-kit', dir: params.dir, document: params.document })
          if (reply !== null) throw new Error('register must return null')
        } else if (call.method === 'hooksprobe.withdraw') {
          const reply = await hooks.withdraw({ plugin: 'foreign-kit' })
          if (reply !== null) throw new Error('withdraw must return null')
        }
        return { kind: 'value', value: { plugins: await hooks.plugins({}), refusal: hooks.refusal({ plugin: 'foreign-kit' }) } }
      },
    )
  },
})
