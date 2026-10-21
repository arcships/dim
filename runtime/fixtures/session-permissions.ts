import { definePlugin } from '@arcships/rutis'
import type { ManagementMethodsContribution, MethodReply, SessionPermissions, SessionsErrorThrown, SessionRef, Sessions } from '../../npm/plugin.js'

let generation = 0
export default definePlugin({
  name: 'SessionPermissionsFixture',
  inject: ['sessionPermissions', 'sessions'],
  provides: { 'managementMethods:permissions': { describe: 'async', call: 'async' } },
  apply(ctx) {
    const applied = ++generation
    const permissions = ctx.use<SessionPermissions>('sessionPermissions')
    const sessions = ctx.use<Sessions>('sessions')
    const methods: ManagementMethodsContribution = {
      async describe() {
        return ['branch', 'defaults', 'legacy'].map(name => ({
          name: `permissions.${name}`, mutating: false, interruption: 'report', progress: false,
        }))
      },
      async call(call): Promise<MethodReply> {
        try {
          // Requests cross the generated decoder used by real process callers.
          const at = call.params as unknown as SessionRef
          if (call.method === 'permissions.legacy') return { kind: 'value', value: JSON.stringify(await sessions.idle(at)) }
          if (call.method === 'permissions.defaults') {
            const policy = await permissions.permissionDefaults({})
            return { kind: 'value', value: { generation: applied, policy: JSON.stringify(policy) } }
          }
          const state = await permissions.permission(at)
          return { kind: 'value', value: { generation: applied, version: state.version.toString(), policy: JSON.stringify(state.policy) } }
        } catch (error) {
          const failure = error as SessionsErrorThrown
          return { kind: 'value', value: { error: {
            name: failure.name, code: failure.code, message: failure.message,
            data: JSON.stringify(failure.data), retryable: 'retryable' in failure,
          } } }
        }
      },
    }
    ctx.provide('managementMethods:permissions', methods)
  },
})
