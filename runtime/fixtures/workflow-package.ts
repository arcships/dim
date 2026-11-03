import { createConnection } from 'node:net'
import { once } from 'node:events'
import { mock } from 'node:test'
import { definePlugin, type Context } from '@arcships/rutis'
import { z } from 'zod'
import workflow from '@dimcode/workflow'
import type { FileError, FileLease, ManagementMethods, PluginFiles, Sessions } from '../../npm/plugin.js'

const control = z.object({ phase: z.enum(['before_input', 'reply_write', 'before_launch', 'stage_input', 'stage_history', 'delete_reply']).optional(), host: z.string().optional(), port: z.number().int().optional(), count: z.number().int().min(1).optional(), failReplace: z.literal('workflow/runs.version').optional(), tick: z.number().int().nonnegative().optional() })

export default definePlugin({
  name: 'WorkflowPackageProbe',
  inject: workflow.inject,
  config: workflow.config,
  provides: workflow.provides,
  async apply(ctx, config) {
    mock.timers.enable({ apis: ['setInterval'] })
    ctx.effect(() => { mock.timers.reset() })
    const native = ctx.use<Sessions>('sessions')
    let selected: z.infer<typeof control> | undefined
    let phase: string | null = null
    let replacementError: FileError | null = null
    let readError: FileError | null = null
    let readFailures = 0
    const pause = async (point: z.infer<typeof control>['phase']) => {
      const gate = selected
      if (gate?.phase !== point || gate.host === undefined || gate.port === undefined) return
      const remaining = (gate.count ?? 1) - 1
      selected = remaining > 0 ? { ...gate, count: remaining } : undefined
      phase = point
      const socket = createConnection({ host: gate.host, port: gate.port })
      ctx.effect(() => { socket.destroy() })
      try {
        await once(socket, 'connect')
        for await (const _ of socket) break
      } finally { phase = null; socket.destroy() }
    }
    const observed: Sessions = {
      async launch(request) { await pause('before_launch'); return native.launch(request) },
      async input(request) {
        if (request.mode === 'steer') await pause('before_input')
        else await pause('stage_input')
        const reply = await native.input(request)
        if (request.mode === 'steer') await pause('reply_write')
        return reply
      },
      lookup: request => native.lookup(request),
      async history(request) { await pause('stage_history'); return native.history(request) },
      withdraw: request => native.withdraw(request), stop: request => native.stop(request),
      idle: request => native.idle(request),
      async delete(request) {
        const reply = await native.delete(request)
        await pause('delete_reply')
        return reply
      },
    }
    const files = ctx.use<PluginFiles>('pluginFiles')
    const observedFiles: PluginFiles = {
      async open() {
        const scope = await files.open()
        return {
          withLock(path, work, cancelled) {
            const execute = (file: FileLease) => work({
              async read() {
                const reply = await file.read()
                if (!reply.ok) { readError = reply.error; readFailures += 1 }
                return reply
              },
              backup: suffix => file.backup(suffix),
              async replace(text) {
                if (selected?.failReplace === path) {
                  selected = undefined
                  replacementError = 'io'
                  return { ok: false as const, error: 'io' as const }
                }
                const reply = await file.replace(text)
                if (!reply.ok) replacementError = reply.error
                return reply
              },
            })
            return cancelled === undefined ? scope.withLock(path, execute) : scope.withLock(path, execute, cancelled)
          },
          quiesce: () => scope.quiesce(), close: () => scope.close(),
        }
      },
    }
    await ctx.use<ManagementMethods>('managementMethods').register(
      'workflowprobe',
      [
        { name: 'workflowprobe.status', mutating: false, interruption: 'report', progress: false },
        { name: 'workflowprobe.configure', mutating: true, interruption: 'drain', progress: false },
      ],
      async call => {
        if (call.method === 'workflowprobe.configure') {
          selected = control.parse(call.params)
          if (selected.tick !== undefined) mock.timers.tick(selected.tick)
        }
        return { kind: 'value', value: { pid: process.pid, phase, replacementError, readError, readFailures } }
      },
    )
    const context: Context = {
      use<T>(name: string): T {
        if (name === 'sessions') return observed as T
        if (name === 'pluginFiles') return observedFiles as T
        return ctx.use<T>(name)
      },
      provide: (name, value) => ctx.provide(name, value), effect: cleanup => ctx.effect(cleanup),
      on: (name, listener) => ctx.on(name, listener), emit: (name, ...args) => ctx.emit(name, ...args),
    }
    await workflow.apply(context, config)
  },
})
