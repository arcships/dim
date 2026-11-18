import { definePlugin } from '@arcships/rutis'
import { partRow } from './part-row.ts'
import type { PromptHost, PromptRegistry, PromptEntry, SessionMethods } from '../../npm/plugin.js'
import type { InitialPromptConfiguration, PromptSourceTarget } from '../../npm/branch-services.js'

function isPromptSourceTarget(value: unknown): value is PromptSourceTarget {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return false
  if (!('source' in value) || typeof value.source !== 'string' || !('scope' in value)) return false
  const scope = value.scope
  return scope !== null && typeof scope === 'object' && !Array.isArray(scope)
    && 'timeline' in scope && typeof scope.timeline === 'string' && 'kind' in scope
    && (scope.kind === 'branch' || ((scope.kind === 'turn' || scope.kind === 'task') && 'id' in scope && typeof scope.id === 'string'))
}

const part = definePlugin({
  name: 'PromptConsumerFixture',
  provides: {},
  inject: ['promptHost', 'promptRegistry', 'initialPromptConfiguration', 'sessionMethods'],
  async apply(ctx) {
    const host = ctx.use<PromptHost>('promptHost')
    const registry = ctx.use<PromptRegistry>('promptRegistry')
    const initial = ctx.use<InitialPromptConfiguration>('initialPromptConfiguration')
    let cancel: () => void = () => {}
    let finished = false
    let pending: Promise<PromptEntry | null> | undefined
    await ctx.use<SessionMethods>('sessionMethods').register(
      'session.promptfixture',
      ['snapshot', 'load', 'start', 'cancel', 'state', 'result', 'duplicate', 'missing', 'abandon'].map(name => ({
        name: `session.promptfixture.${name}`, mutating: false, interruption: 'report', progress: false,
      })),
      async call => {
        const action = call.method.slice('session.promptfixture.'.length)
        if (action === 'snapshot') {
          const names = registry.names()
          if (!Array.isArray(names)) throw new Error('promptRegistry.names must return a plain array')
          return { kind: 'value', value: { names, initial: await initial.get() } }
        }
        if (action === 'cancel') { cancel(); return { kind: 'value', value: null } }
        if (action === 'state') return { kind: 'value', value: { finished } }
        if (action === 'result') {
          try { return { kind: 'value', value: await pending } }
          catch (error) {
            if (!(error instanceof Error) || !('code' in error) || typeof error.code !== 'string') throw error
            return { kind: 'value', value: { error: { code: error.code, message: error.message } } }
          }
        }
        if (action === 'duplicate' || action === 'missing') {
          try {
            if (action === 'duplicate') await registry.register('fixture', async () => null)
            else await host.load({ source: 'missing', scope: { kind: 'branch', timeline: 'main' } }, () => new Promise(() => {}))
            throw new Error('the operation unexpectedly succeeded')
          } catch (error) {
            if (!(error instanceof Error) || !('code' in error) || typeof error.code !== 'string') throw error
            return { kind: 'value', value: { code: error.code, message: error.message } }
          }
        }
        const target = call.params.target
        if (!isPromptSourceTarget(target)) throw new Error('invalid prompt target')
        if (action === 'abandon') {
          void host.load(target, () => new Promise(() => {}))
          return { kind: 'value', value: null }
        }
        if (action === 'load') return { kind: 'value', value: await host.load(target, () => new Promise(() => {})) }
        if (action === 'start') {
          finished = false
          const cancelled = new Promise<void>(resolve => { cancel = resolve })
          pending = host.load(target, () => cancelled).finally(() => { finished = true })
          void pending.catch(() => {})
          return { kind: 'value', value: null }
        }
        throw new Error(`unknown operation ${action}`)
      },
    )
  },
})

export default partRow('session', part)
