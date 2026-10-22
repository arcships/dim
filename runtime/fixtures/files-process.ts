import { createConnection } from 'node:net'
import { once } from 'node:events'
import { definePlugin } from '@arcships/rutis'
import type { PluginFiles, ManagementMethodsContribution } from '../../npm/plugin.js'

export default definePlugin({
  name: 'PublicFilesProcessFixture',
  inject: ['pluginFiles'],
  provides: { 'managementMethods:files': { describe: 'async', call: 'async' } },
  async apply(ctx, config: { gate?: { host: string; port: number } }) {
    const scope = await ctx.use<PluginFiles>('pluginFiles').open()
    ctx.effect(() => scope.close())
    const methods: ManagementMethodsContribution = {
      async describe() {
        return ['read', 'write', 'hold', 'close', 'missing', 'invalid'].map(name => ({
          name: `files.${name}`, mutating: ['write', 'hold', 'close'].includes(name),
          interruption: 'drain', progress: name === 'hold',
        }))
      },
      async call(call, _cancelled, progress) {
        if (call.method === 'files.close') {
          await scope.close()
          return { kind: 'value', value: null }
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
    }
    ctx.provide('managementMethods:files', methods)
  },
})
