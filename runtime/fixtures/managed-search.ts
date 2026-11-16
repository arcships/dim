import { definePlugin } from '@arcships/rutis'
import type { ManagedSearch, ManagementMethods } from '../../npm/plugin.js'

export default definePlugin({
  name: 'ManagedSearchFixture',
  provides: {},
  inject: ['managementMethods', 'managedSearch'],
  async apply(ctx) {
    const search = ctx.use<ManagedSearch>('managedSearch')
    await ctx.use<ManagementMethods>('managementMethods').register(
      'searchprobe',
      [{ name: 'searchprobe.search', mutating: false, interruption: 'report', progress: false }],
      async (call: { params: { query: string; count: number } }) => {
        try {
          return { kind: 'value', value: { reply: await search.search(call.params) } }
        } catch (error) {
          if (!(error instanceof Error) || !('code' in error)) throw error
          return { kind: 'value', value: { error: { code: error.code, message: error.message } } }
        }
      },
    )
  },
})
