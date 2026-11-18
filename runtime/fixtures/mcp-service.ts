import { definePlugin } from '@arcships/rutis'
import type { ManagementMethods, McpService } from '../../npm/plugin.js'

export default definePlugin({
  name: 'McpServiceFixture',
  provides: {},
  inject: ['managementMethods', 'mcpService'],
  async apply(ctx) {
    const mcp = ctx.use<McpService>('mcpService')
    await ctx.use<ManagementMethods>('managementMethods').register(
      'mcpprobe',
      ['register', 'read', 'withdraw'].map(action => ({
        name: `mcpprobe.${action}`, mutating: action !== 'read', interruption: 'report', progress: false,
      })),
      async call => {
        if (call.method === 'mcpprobe.register') {
          const params = call.params
          if (params === null || typeof params !== 'object' || Array.isArray(params) || typeof params.dir !== 'string' || params.document === undefined) throw new Error('expected dir and document')
          const result = await mcp.register({ plugin: 'foreign-kit', dir: params.dir, document: params.document })
          if (result !== null) throw new Error('register must return null')
        } else if (call.method === 'mcpprobe.withdraw') {
          const result = await mcp.withdraw({ plugin: 'foreign-kit' })
          if (result !== null) throw new Error('withdraw must return null')
        }
        return { kind: 'value', value: { plugins: await mcp.plugins({}), refusal: mcp.refusal({ plugin: 'foreign-kit' }) } }
      },
    )
  },
})
