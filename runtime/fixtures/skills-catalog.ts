import { definePlugin } from '@arcships/rutis'
import type { ManagementMethods, SkillsCatalog } from '../../npm/plugin.js'

export default definePlugin({
  name: 'SkillsCatalogFixture',
  provides: {},
  inject: ['managementMethods', 'skillsCatalog'],
  async apply(ctx) {
    const skills = ctx.use<SkillsCatalog>('skillsCatalog')
    await ctx.use<ManagementMethods>('managementMethods').register(
      'skillprobe',
      ['register', 'read', 'withdraw'].map(action => ({
        name: `skillprobe.${action}`, mutating: action !== 'read', interruption: 'report', progress: false,
      })),
      async call => {
        if (call.method === 'skillprobe.register') {
          const params = call.params
          if (params === null || typeof params !== 'object' || Array.isArray(params) || typeof params.root !== 'string') throw new Error('expected root')
          return { kind: 'value', value: await skills.register({ plugin: 'foreign-kit', root: params.root }) }
        }
        if (call.method === 'skillprobe.withdraw') {
          const result = await skills.withdraw({ plugin: 'foreign-kit' })
          if (result !== null) throw new Error('withdraw must return null')
        }
        return { kind: 'value', value: { plugins: await skills.plugins({}), issues: await skills.issues({ plugin: 'foreign-kit' }) } }
      },
    )
  },
})
