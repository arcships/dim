import { definePlugin } from '@arcships/rutis'
import { partRow } from './part-row.ts'
import type { CommandRules, MethodReply, SessionMethods } from '../../npm/plugin.js'
import type { SessionIdentity } from '../../npm/branch-services.js'

export const part = definePlugin({
  name: 'CommandRulesFixture',
  provides: {},
  inject: ['commandRules', 'sessionMethods', 'sessionIdentity'],
  async apply(ctx) {
    const rules = ctx.use<CommandRules>('commandRules')
    const identity = await ctx.use<SessionIdentity>('sessionIdentity').get()
    await ctx.use<SessionMethods>('sessionMethods').register(
      'session.commandrules',
      [{ name: 'session.commandrules.read', mutating: false, interruption: 'report', progress: false }],
      async (): Promise<MethodReply> => {
        try {
          return { kind: 'value', value: { sessionId: identity.sessionId, reply: await rules.read({}) } }
        } catch (error) {
          if (!(error instanceof Error) || !('code' in error) || typeof error.code !== 'string') throw error
          return { kind: 'value', value: { sessionId: identity.sessionId, error: { code: error.code, message: error.message } } }
        }
      },
    )
  },
})

export default partRow('session', part)
