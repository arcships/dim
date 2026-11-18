import { definePlugin } from '@arcships/rutis'
import type { FsHost, ManagementMethods } from '../../npm/plugin.js'
import assert from 'node:assert/strict'

export default definePlugin({
  name: 'FsReaderFixture',
  provides: {},
  inject: ['managementMethods', 'fsHost'],
  async apply(ctx) {
    const files = ctx.use<FsHost>('fsHost')
    await ctx.use<ManagementMethods>('managementMethods').register(
      'fsreader',
      ['status', 'open', 'read', 'close'].map(name => ({ name: `fsreader.${name}`, mutating: true, interruption: 'report', progress: false })),
      async (call: { method: string; params: any }) => {
        if (call.method === 'fsreader.status') {
          await assert.rejects(() => Reflect.apply(files.openRead, files, [['/unused']]), /request must be an object/)
          return { kind: 'value', value: { pid: process.pid } }
        }
        const value = call.method === 'fsreader.open' ? await files.openRead(call.params)
          : call.method === 'fsreader.read' ? await files.readAt(call.params)
          : await files.closeRead(call.params)
        return { kind: 'value', value }
      },
    )
  },
})
