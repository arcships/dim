import { definePlugin } from '@arcships/rutis'
import provides from './settings-owner.json' with { type: 'json' }
import sources from './settings-owner.settings.json' with { type: 'json' }
import type { JsonValue, PartRegistry, SessionMethods, Settings as SettingsService, SourceDeclaration } from '../../npm/plugin.js'

type Envelope = { ok: boolean; value?: JsonValue; error?: { code: string; message: string } }
type Settings = { read(request: unknown): Promise<Envelope>; update(request: unknown): Promise<Envelope> }
type MethodCall = { method: string; params: { sessionId: string; [key: string]: unknown } }

// The owner of source `example` (settings-service §11): it reads and writes
// its document through `settings`; each method returns the envelope unchanged.
// It also records the Settings events it listens to on the bus (§4.5).
export const part = definePlugin({
  name: 'SettingsOwnerFixture',
  provides,
  inject: ['settings', 'sessionMethods'],
  async apply(ctx) {
    const settings = ctx.use<Settings>('settings')
    let pauseDecode: (() => Promise<void>) | undefined
    const registerCustom = () => ctx.use<SettingsService>('settings').registerDocument(
      { source: 'example-custom', userFile: 'example-custom.json', projectFile: '.example-custom.json' },
      {
        async decode(bytes): Promise<{ count: number }> {
          await pauseDecode?.()
          return bytes === null ? { count: 3 } : JSON.parse(new TextDecoder().decode(bytes))
        },
        validate(document: { count: number }) {
          if (!Number.isInteger(document.count) || document.count < 0) throw new Error('invalid count')
        },
      },
    )
    let custom: Awaited<ReturnType<typeof registerCustom>>
    const events: { name: string; payload: JsonValue }[] = []
    for (const name of ['settings/example', 'settings/projects']) {
      ctx.on(name, (payload: JsonValue) => { events.push({ name, payload }) })
    }
    await ctx.use<SessionMethods>('sessionMethods').register(
      'session.example',
      ['read', 'update', 'events', 'declared', 'custom_read', 'custom_update', 'custom_concurrent'].map(name => ({
        name: `session.example.${name}`, mutating: false, interruption: 'report', progress: false,
      })),
      async (call: MethodCall) => {
        const { sessionId: _, ...request } = call.params
        if (call.method.endsWith('.custom_read')) {
          custom ??= await registerCustom()
          return { kind: 'value', value: await custom.read('user') }
        }
        if (call.method.endsWith('.custom_concurrent')) {
          if (typeof request.root !== 'number') throw new Error('expected project root')
          const scope = { project: request.root }
          const original = await custom.read(scope)
          if (!original.ok) throw new Error(original.error.message)
          let entered!: () => void
          let release!: () => void
          const decoding = new Promise<void>(resolve => { entered = resolve })
          const released = new Promise<void>(resolve => { release = resolve })
          pauseDecode = () => { entered(); return released }
          const reader = Promise.resolve(custom.read(scope))
          try {
            await decoding
            const stale = await custom.update(scope, 'stale', { count: 7 })
            const written = await custom.update(scope, original.value.revision, { count: 8 })
            return { kind: 'value', value: { stale, written } }
          } finally {
            pauseDecode = undefined
            release()
            await reader
          }
        }
        if (call.method.endsWith('.custom_update')) {
          if (typeof request.expectedRevision !== 'string' || typeof request.count !== 'number') throw new Error('expected revision and count')
          return { kind: 'value', value: await custom.update('user', request.expectedRevision, { count: request.count }) }
        }
        if (call.method.endsWith('.declared')) {
          if (typeof request.instanceId !== 'string') throw new Error('expected instanceId')
          return { kind: 'value', value: ctx.use<SettingsService>('settings').declaredBy({ instanceId: request.instanceId }) }
        }
        if (call.method.endsWith('.events')) return { kind: 'value', value: events }
        const reply = call.method.endsWith('.read') ? await settings.read(request) : await settings.update(request)
        return { kind: 'value', value: reply }
      },
    )
  },
})

export default definePlugin<{ settings?: SourceDeclaration[] }>({
  name: 'SettingsOwner',
  inject: ['settings', 'partRegistry'],
  async apply(ctx, config) {
    const settings = ctx.use<SettingsService>('settings')
    for (const declaration of config?.settings ?? sources as SourceDeclaration[]) await settings.register(declaration)
    await ctx.use<PartRegistry>('partRegistry').registerDefinition('session', 'SettingsOwnerFixture', part)
  },
})
