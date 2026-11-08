import { createConnection } from 'node:net'
import { once } from 'node:events'
import { definePlugin } from '@arcships/rutis'
import type { ManagementMethods, PluginFiles } from '../../npm/plugin.js'

export default definePlugin({
  name: 'PublicFilesProcessFixture',
  inject: ['pluginFiles', 'managementMethods'],
  provides: {},
  async apply(ctx, config: { gate?: { host: string; port: number } }) {
    const scope = await ctx.use<PluginFiles>('pluginFiles').open()
    ctx.effect(() => scope.close())
    await ctx.use<ManagementMethods>('managementMethods').register(
      'files',
      ['read', 'write', 'hold', 'close', 'missing', 'invalid', 'bytes', 'envelope', 'list', 'remove', 'invalid_names'].map(name => ({
        name: `files.${name}`,
        mutating: ['write', 'hold', 'close', 'bytes', 'remove'].includes(name),
        interruption: 'drain', progress: name === 'hold',
      })),
      async (call, _cancelled, progress) => {
        if (call.method === 'files.close') {
          await scope.close()
          return { kind: 'value', value: null }
        }
        if (call.method === 'files.bytes') {
          const result = await scope.withLock('public-files/blob.bin', async file => {
            const replaced = await file.replaceBytes(new Uint8Array([0, 1, 2, 250, 255, 128]))
            if (!replaced.ok) return replaced
            const read = await file.readBytes()
            if (!read.ok) return read
            return { ok: true, value: read.value === null ? null : Array.from(read.value) }
          })
          return { kind: 'value', value: result }
        }
        if (call.method === 'files.envelope') {
          const result = await scope.withLock('public-files/state.json', async file => {
            const read = await file.readBytes()
            return read.ok && read.value
              ? { ok: true, value: new TextDecoder().decode(read.value) }
              : read
          })
          return { kind: 'value', value: result }
        }
        if (call.method === 'files.list') {
          return { kind: 'value', value: await scope.list('public-files') }
        }
        if (call.method === 'files.remove') {
          return { kind: 'value', value: await scope.remove('public-files/blob.bin') }
        }
        if (call.method === 'files.invalid_names') {
          return {
            kind: 'value',
            value: [
              await scope.list('../escape'),
              await scope.remove('/absolute'),
              await scope.list(''),
            ],
          }
        }
        const result = call.method === 'files.missing'
          ? await scope.withLock('unprovisioned/runs.json', () => { throw new Error('missing parent entered callback') })
          : await scope.withLock(call.method === 'files.invalid' ? 'public-files/invalid.json' : 'public-files/state.json', async file => {
            if (call.method === 'files.hold') {
              if (!config.gate) throw new Error('fixture requires a gate')
              const socket = createConnection(config.gate)
              ctx.effect(() => { socket.destroy() })
              try {
                await once(socket, 'connect')
                progress({ phase: 'started' })
                await once(socket, 'data')
                return await file.replace('settled public process text')
              } finally { socket.destroy() }
            }
            return call.method === 'files.write'
              ? await file.replace('public process text')
              : await file.read()
          })
        return { kind: 'value', value: result }
      },
    )
  },
})
