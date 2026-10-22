import { createConnection } from 'node:net'
import { once } from 'node:events'
import { definePlugin } from '@arcships/rutis'
import type { PluginFiles, SessionMethodsContribution } from '../../npm/plugin.js'

export default definePlugin({
  name: 'PublicFilesSessionFixture',
  inject: ['pluginFiles'],
  provides: { 'sessionMethods:session.files': { describe: 'async', call: 'async' } },
  async apply(ctx, config: { gate: { host: string; port: number } }) {
    const scope = await ctx.use<PluginFiles>('pluginFiles').open()
    ctx.effect(() => scope.close())
    const methods: SessionMethodsContribution = {
      async describe() {
        return ['read', 'write', 'hold'].map(name => ({
          name: `session.files.${name}`, mutating: name !== 'read',
          interruption: 'drain', progress: false,
        }))
      },
      async call(call) {
        if (call.method === 'session.files.hold') {
          let started!: () => void
          const connected = new Promise<void>(resolve => { started = resolve })
          // The scope owns this transaction after the admitted method returns.
          void scope.withLock('public-files/state.json', async file => {
            const socket = createConnection(config.gate)
            try {
              await once(socket, 'connect')
              started()
              await once(socket, 'data')
              return await file.replace('settled public Session text')
            } finally { socket.destroy() }
          })
          await connected
          return { kind: 'value', value: null }
        }
        const result = await scope.withLock('public-files/state.json', async file =>
          call.method === 'session.files.write'
            ? await file.replace('public Session text')
            : await file.read())
        return { kind: 'value', value: result }
      },
    }
    ctx.provide('sessionMethods:session.files', methods)
  },
})
