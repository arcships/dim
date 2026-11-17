import { definePlugin } from '@arcships/rutis'
import type { ManagementMethods, SessionFiles } from '../../npm/plugin.js'

export default definePlugin({
  name: 'SessionFilesFixture',
  provides: {},
  inject: ['managementMethods', 'sessionFiles'],
  async apply(ctx) {
    const files = ctx.use<SessionFiles>('sessionFiles')
    await ctx.use<ManagementMethods>('managementMethods').register(
      'sessionfilesprobe',
      [{ name: 'sessionfilesprobe.exercise', mutating: true, interruption: 'drain', progress: false }],
      async (call: { params: { sessionId: string; otherSessionId: string; source: string; output: string } }) => {
        const { sessionId, otherSessionId, source, output } = call.params
        const put = await files.put({ sessionId, name: 'wire.txt', content: Buffer.from('wire text').toString('base64') })
        const imported = await files.import({ sessionId, name: 'imported.bin', source })
        const first = await files.read({ sessionId, fileId: imported.fileId, offset: 0 })
        const end = await files.read({ sessionId, fileId: imported.fileId, offset: imported.bytes })
        await files.register({ sessionId, path: output })
        const owned = await files.owns({ sessionId, path: output })
        const paths = await files.owned_paths({ sessionId })
        let foreignError: string | undefined
        try {
          await files.read({ sessionId: otherSessionId, fileId: imported.fileId, offset: 0 })
        } catch (error) {
          if (!(error instanceof Error) || !('code' in error) || typeof error.code !== 'string') throw error
          foreignError = error.code
        }
        return { kind: 'value', value: { put, imported, first, end, owned, paths, foreignError } }
      },
    )
  },
})
