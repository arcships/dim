import { definePlugin } from '@arcships/rutis'
import { partRow } from './part-row.ts'
import provides from './session-metadata.json' with { type: 'json' }
import type { JsonValue, SessionMethods } from '../../npm/plugin.js'

type Envelope = { ok: boolean; value?: JsonValue; error?: { code: string; message: string } }
type SessionMetadata = { read(request: unknown): Promise<Envelope>; setTitle(request: unknown): Promise<Envelope> }
type MethodCall = { method: string; params: { sessionId: string; [key: string]: unknown } }

// Uses `sessionMetadata` as SessionTitle does (session-title-metadata §12):
// each method passes its Session's request through and returns the envelope
// unchanged.
export const part = definePlugin({
  name: 'SessionMetadataFixture',
  provides,
  inject: ['sessionMetadata', 'sessionMethods'],
  async apply(ctx) {
    const metadata = ctx.use<SessionMetadata>('sessionMetadata')
    await ctx.use<SessionMethods>('sessionMethods').register(
      'session.titles',
      ['read', 'set'].map(name => ({
        name: `session.titles.${name}`, mutating: false, interruption: 'report', progress: false,
      })),
      async (call: MethodCall) => {
        const reply = call.method.endsWith('.read')
          ? await metadata.read({ sessionId: call.params.sessionId })
          : await metadata.setTitle(call.params)
        return { kind: 'value', value: reply }
      },
    )
  },
})

export default partRow('session', part)
