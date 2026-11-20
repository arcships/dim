import { definePlugin } from '@arcships/rutis'
import type { ManagementMethods, MethodReply, SessionEnded, SessionsErrorThrown, SessionRef, Sessions } from '../../npm/plugin.js'

let generation = 0
export default definePlugin({
  name: 'SessionPermissionsFixture',
  inject: ['sessions', 'managementMethods'],
  provides: {},
  async apply(ctx) {
    const applied = ++generation
    const ends: SessionEnded[] = []
    const sessions = ctx.use<Sessions>('sessions')
    await ctx.use<ManagementMethods>('managementMethods').register(
      'permissions',
      ['branch', 'defaults', 'legacy', 'sources', 'observe', 'ends'].map(name => ({
        name: `permissions.${name}`, mutating: false, interruption: 'report', progress: false,
      })),
      async (call): Promise<MethodReply> => {
        try {
          const at = call.params as unknown as SessionRef
          if (call.method === 'permissions.ends') return { kind: 'value', value: JSON.stringify(ends) }
          if (call.method === 'permissions.observe') {
            const request = call.params as unknown as Parameters<Sessions['observeCurrent']>[0]
            const observed = await sessions.observeCurrent(request, end => { ends.push(end) })
            return { kind: 'value', value: observed }
          }
          if (call.method === 'permissions.sources') {
            const request = call.params as unknown as Parameters<Sessions['startupSources']>[0]
            return { kind: 'value', value: JSON.stringify(await sessions.startupSources(request)) }
          }
          if (call.method === 'permissions.legacy') return { kind: 'value', value: JSON.stringify(await sessions.idle(at)) }
          if (call.method === 'permissions.defaults') {
            const policy = await sessions.permissionDefaults({})
            return { kind: 'value', value: { generation: applied, policy: JSON.stringify(policy) } }
          }
          const state = await sessions.permission(at)
          return { kind: 'value', value: { generation: applied, version: state.version.toString(), policy: JSON.stringify(state.policy) } }
        } catch (error) {
          const failure = error as SessionsErrorThrown
          return { kind: 'value', value: { error: {
            name: failure.name, code: failure.code, message: failure.message,
            data: JSON.stringify(failure.data), retryable: 'retryable' in failure,
          } } }
        }
      },
    )
  },
})
