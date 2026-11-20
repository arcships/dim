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
      ['status', 'resolve', 'glob', 'grep', 'stat', 'open', 'read', 'close', 'mkdir', 'list', 'remove', 'lock', 'unlock', 'replace', 'backup', 'read_bytes'].map(name => ({ name: `fsreader.${name}`, mutating: true, interruption: 'report', progress: false })),
      async (call: { method: string; params: any }) => {
        if (call.method === 'fsreader.status') {
          await assert.rejects(() => Reflect.apply(files.openRead, files, [['/unused']]), /request must be an object/)
          return { kind: 'value', value: { pid: process.pid } }
        }
        if (call.method === 'fsreader.resolve') return {kind: 'value', value: await files.resolve(call.params)}
        if (call.method === 'fsreader.glob' || call.method === 'fsreader.grep') {
          const { result } = await (call.method === 'fsreader.glob' ? files.glob(call.params) : files.grep(call.params))
          return { kind: 'value', value: result.structuredContent ?? null }
        }
        if (call.method === 'fsreader.stat') return { kind: 'value', value: await files.stat(call.params) }
        if (call.method === 'fsreader.lock') return { kind: 'value', value: await files.lock(call.params) }
        if (call.method === 'fsreader.unlock') return { kind: 'value', value: await files.unlock(call.params) }
        if (call.method === 'fsreader.replace') return { kind: 'value', value: await files.replace(call.params) }
        if (call.method === 'fsreader.backup') return { kind: 'value', value: await files.backup(call.params) }
        if (call.method === 'fsreader.read_bytes') {
          try { return { kind: 'value', value: await files.readBytes(call.params) } }
          catch (error) {
            assert.ok(error instanceof Error && 'code' in error && typeof error.code === 'string')
            return { kind: 'value', value: { code: error.code } }
          }
        }
        if (call.method === 'fsreader.mkdir') return { kind: 'value', value: await files.createDirectory(call.params) }
        if (call.method === 'fsreader.list') return { kind: 'value', value: await files.list(call.params) }
        if (call.method === 'fsreader.remove') return { kind: 'value', value: await files.remove(call.params) }
        const value = call.method === 'fsreader.open' ? await files.openRead(call.params)
          : call.method === 'fsreader.read' ? await files.readAt(call.params)
          : await files.closeRead(call.params)
        return { kind: 'value', value }
      },
    )
  },
})
