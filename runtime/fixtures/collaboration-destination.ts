import { definePlugin } from '@arcships/rutis'
import { partRow } from './part-row.ts'
import type { CollaborationSession, SessionMethods } from '../../npm/plugin.js'

export const part = definePlugin({
  name: 'CollaborationDestinationFixture',
  provides: {},
  inject: ['collaborationSession', 'sessionMethods'],
  async apply(ctx) {
    const collaboration = ctx.use<CollaborationSession>('collaborationSession')
    await ctx.use<SessionMethods>('sessionMethods').register(
      'session.collaborationface',
      [{ name: 'session.collaborationface.destination', mutating: false, interruption: 'report', progress: false }],
      async call => {
        const params = call.params
        if (params === null || typeof params !== 'object' || Array.isArray(params)
          || typeof params.memberSessionId !== 'string') {
          throw new Error('expected memberSessionId')
        }
        return { kind: 'value', value: collaboration.approvalDestination({ memberSessionId: params.memberSessionId }) }
      },
    )
  },
})

export default partRow('session', part)
