import { definePlugin } from '@arcships/rutis'
import { partRow } from './part-row.ts'
import type { Services, SessionMethods } from '../../npm/plugin.js'

export const part = definePlugin({
  name: 'ObjectFacesFixture',
  inject: ['mcpSession', 'backgroundExec', 'sessionMethods'],
  provides: {},
  async apply(ctx) {
    const mcp = ctx.use<Services['mcpSession']>('mcpSession')
    const background = ctx.use<Services['backgroundExec']>('backgroundExec')
    await ctx.use<SessionMethods>('sessionMethods').register(
      'session.objectface',
      [{ name: 'session.objectface.call', mutating: true, interruption: 'report', progress: false }],
      async (call: { params:
        | { operation: 'state' | 'missingServer' }
        | { operation: 'output' | 'outputPage' | 'wait' | 'stop'; taskId: string }
      }) => {
        try {
          let value: unknown
          switch (call.params.operation) {
            case 'state': value = await mcp.state(); break
            case 'missingServer': value = await mcp.call('absent', 'tool', {}); break
            case 'output': value = await background.output(call.params.taskId); break
            case 'outputPage': value = await background.outputPage(call.params.taskId, 0, 64); break
            case 'wait': value = await background.wait(call.params.taskId, 0); break
            case 'stop': value = await background.stop(call.params.taskId); break
            default: throw new Error('unknown fixture operation')
          }
          return { kind: 'value', value }
        } catch (error) {
          if (!(error instanceof Error) || !('code' in error)) throw error
          return { kind: 'value', value: { error: { code: error.code, message: error.message } } }
        }
      },
    )
  },
})

export default partRow('branch', part)
