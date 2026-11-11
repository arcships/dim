import { definePlugin } from '@arcships/rutis'
import { partRow } from './part-row.ts'
import type { SessionFiles, SessionMethods } from '../../npm/plugin.d.ts'

/** 11 MiB + 1 decoded bytes, the first size a put must refuse. */
const OVER_LIMIT = 'A'.repeat(4 * 3_844_779)

function failure(error: unknown) {
  if (!(error instanceof Error) || !('code' in error)) throw error
  return { code: (error as { code: string }).code, message: error.message }
}

export const part = definePlugin({
  name: 'SessionFilesFixture',
  inject: ['sessionFiles', 'sessionMethods'],
  provides: {},
  async apply(ctx) {
    const files = ctx.use<SessionFiles>('sessionFiles')
    await ctx.use<SessionMethods>('sessionMethods').register(
      'session.files',
      [{ name: 'session.files.probe', mutating: false, interruption: 'report', progress: false }],
      async (call: { params: { sessionId: string } }) => {
        const sessionId = call.params.sessionId
        const stored = await files.put({ sessionId, name: 'probe.txt', content: btoa('probe') })
        const owned = (await files.owns({ sessionId, path: stored.path })).owned
        let large: unknown = null
        try {
          await files.put({ sessionId, name: 'large.txt', content: OVER_LIMIT })
        } catch (error) {
          large = failure(error)
        }
        let missing: unknown = null
        try {
          await files.put({ sessionId: 'missing', name: 'probe.txt', content: btoa('probe') })
        } catch (error) {
          missing = failure(error)
        }
        return { kind: 'value', value: { stored, owned, large, missing } }
      },
    )
  },
})

export default partRow('session', part)
