import { createConnection } from 'node:net'
import { once } from 'node:events'
import { definePlugin, type Plugin } from '@arcships/rutis'
import type { ManagementMethods, MethodDeclaration, Settings, SourceDeclaration } from '../../npm/plugin.js'
import provides from './process-part.json' with { type: 'json' }

let applied = 0
export default definePlugin({
  name: 'ProcessTypeScriptFixture',
  provides: provides as Plugin['provides'],
  inject: ['settings', 'managementMethods'],
  async apply(ctx, config: {
    failAfter?: number; badMethods?: boolean;
    holdApply?: { host: string; port: number };
    declarations?: SourceDeclaration[];
  }) {
    const generation = ++applied
    if (config.failAfter && generation >= config.failAfter) throw new Error('fixture apply failure')
    const settings = ctx.use<Settings>('settings')
    // The sources the row's config names, registered as a plugin's are.
    for (const declaration of config.declarations ?? []) await settings.register(declaration)
    let stopped = false
    let calls = 0
    const pending = new Set<Promise<void>>()
    ctx.effect(async () => {
      // Cancellation is asynchronous; keep owned state until accepted calls settle.
      await Promise.all(pending)
      stopped = true
    })
    const alive = () => { if (stopped) throw new Error('original row already disposed') }
    const hold = async (address: { host: string; port: number }) => {
      const socket = createConnection(address)
      ctx.effect(() => { socket.destroy() })
      await once(socket, 'data')
    }
    if (config.holdApply) await hold(config.holdApply)
    const status = () => ({ generation, pid: process.pid, calls, alive: !stopped })
    ctx.provide('exampleProcess', { status() { alive(); return status() } })
    // A method outside the namespace is refused, which fails the row.
    const declarations: MethodDeclaration[] = config.badMethods
      ? [{ name: 'outside.status', mutating: false, interruption: 'report', progress: false }]
      : ['status', 'read', 'error', 'malformed', 'undefined', 'report', 'drain'].map(name => ({
        name: `example.${name}`, mutating: false,
        interruption: name === 'drain' ? 'drain' : 'report', progress: true,
      }))
    await ctx.use<ManagementMethods>('managementMethods').register(
      'example', declarations,
      async (call, cancelled, progress) => {
        let done!: () => void
        const settled = new Promise<void>(resolve => { done = resolve })
        pending.add(settled)
        try {
          alive()
          calls++
          if (call.method === 'example.error') return { code: 'fixture_error', message: 'fixture error' }
          // Exercise malformed foreign data at the actual native boundary.
          if (call.method === 'example.malformed') return { extra: true } as never
          if (call.method === 'example.undefined') return { kind: 'value' }
          if (call.method === 'example.read') {
            const reply = await settings.read({ source: 'example', scope: 'user' })
            if (!reply.ok) return { code: reply.error.code, message: reply.error.message }
            return { kind: 'value', value: reply.value.document }
          }
          progress({ phase: 'started', generation })
          const held = call.method === 'example.report' || call.method === 'example.drain'
          if (held) await cancelled
          alive()
          progress({ phase: 'settled', generation })
          return { kind: 'value', value: { ...status(), params: call.params, callId: call.callId, cancelled: held } }
        } finally {
          pending.delete(settled)
          done()
        }
      },
    )
  },
})
