import { createConnection } from 'node:net'
import { once } from 'node:events'
import { mock } from 'node:test'
import { definePlugin, type Context } from '@arcships/rutis'
import { z } from 'zod'
import scheduler from '@dimcode/scheduler'
import type { Run } from '../plugins/scheduler/store.ts'
import type { FileError, FileLease, ManagementMethods, PluginFiles, SessionPermissions, Sessions, SessionsError, Settings, SettingsErrorCode } from '../../npm/plugin.js'

const gate = z.strictObject({
  phase: z.enum(['launch_reply', 'binding_before', 'binding_reply', 'input_before', 'input_reply', 'report_input', 'report_reply', 'reuse_reply', 'policy_reply', 'history_reply', 'launching_commit', 'delete_reply']),
  host: z.string(), port: z.number().int(),
})
const controls = z.strictObject({ now: z.iso.datetime().optional(), gate: gate.optional(), loseReportReply: z.boolean().optional() })

// Exercise the real package through its public ports, keeping every native call on its original row.
export default definePlugin({
  name: 'SchedulerPackageProbe',
  inject: scheduler.inject, config: scheduler.config,
  provides: scheduler.provides,
  async apply(ctx, config) {
    let selected: z.infer<typeof gate> | undefined
    let clock = false
    let replacementError: FileError | null = null
    let readError: FileError | null = null
    let settingsReadError: SettingsErrorCode | null = null
    let lookupError: SessionsError | null = null
    let loseReportReply = false
    let reportAttempts = 0
    ctx.effect(() => { if (clock) mock.timers.reset() })
    const pause = async (point: z.infer<typeof gate>['phase']) => {
      const next = selected
      if (next?.phase !== point) return
      selected = undefined
      const socket = createConnection({ host: next.host, port: next.port })
      ctx.effect(() => { socket.destroy() })
      try {
        await once(socket, 'connect')
        for await (const _ of socket) break
      } finally { socket.destroy() }
    }
    const native = ctx.use<Sessions>('sessions')
    const observed: Sessions = {
      async launch(request) {
        const reply = await native.launch(request)
        await pause('launch_reply')
        return reply
      },
      async input(request) {
        if (request.mode === 'steer') reportAttempts++
        await pause(request.mode === 'steer' ? 'report_input' : 'input_before')
        const reply = await native.input(request)
        await pause(request.mode === 'steer' ? 'report_reply' : 'input_reply')
        if (request.mode === 'steer' && reply.ok && loseReportReply) {
          loseReportReply = false
          throw new Error('Report reply lost after native publication')
        }
        return reply
      },
      async lookup(request) {
        const reply = await native.lookup(request)
        if (!reply.ok) lookupError = reply.error
        return reply
      },
      async history(request) { const reply = await native.history(request); await pause('history_reply'); return reply },
      withdraw: request => native.withdraw(request), stop: request => native.stop(request),
      idle: request => native.idle(request),
      async delete(request) {
        const reply = await native.delete(request)
        await pause('delete_reply')
        return reply
      },
    }
    const permissions = ctx.use<SessionPermissions>('sessionPermissions')
    const observedPermissions: SessionPermissions = {
      async permission(request) { const reply = await permissions.permission(request); await pause('reuse_reply'); return reply },
      async permissionDefaults(request) { const reply = await permissions.permissionDefaults(request); await pause('policy_reply'); return reply },
    }
    const files = ctx.use<PluginFiles>('pluginFiles')
    const settings = ctx.use<Settings>('settings')
    const observedSettings: Settings = {
      async read(request) {
        const reply = await settings.read(request)
        if (!reply.ok) settingsReadError = reply.error.code
        return reply
      },
      async update(request) {
        await pause('binding_before')
        const reply = await settings.update(request)
        await pause('binding_reply')
        return reply
      },
      projects: request => settings.projects(request),
      register: declaration => settings.register(declaration),
    }
    const observedFiles: PluginFiles = {
      async open() {
        const scope = await files.open()
        return {
          withLock(path, work, cancelled) {
            const execute = (file: FileLease) => work({
              async read() {
                const reply = await file.read()
                if (!reply.ok) readError = reply.error
                return reply
              },
              backup: suffix => file.backup(suffix),
              async replace(text) {
                const reply = await file.replace(text)
                if (!reply.ok) replacementError = reply.error
                if (reply.ok && selected?.phase === 'launching_commit'
                  && JSON.parse(text).runs.some((run: Run) => run.state === 'launching' && !run.session_id)) await pause('launching_commit')
                return reply
              },
            })
            return cancelled === undefined ? scope.withLock(path, execute) : scope.withLock(path, execute, cancelled)
          },
          quiesce: () => scope.quiesce(),
          close: () => scope.close(),
        }
      },
    }
    await ctx.use<ManagementMethods>('managementMethods').register(
      'schedulerprobe', [
        { name: 'schedulerprobe.status', mutating: false, interruption: 'report', progress: false },
        { name: 'schedulerprobe.configure', mutating: true, interruption: 'drain', progress: false },
      ],
      async call => {
        if (call.method === 'schedulerprobe.configure') {
          const input = controls.parse(call.params)
          if (input.now !== undefined) {
            const now = Date.parse(input.now)
            if (clock) mock.timers.setTime(now)
            else { mock.timers.enable({ apis: ['Date'], now }); clock = true }
          }
          if (input.gate) selected = input.gate
          if (input.loseReportReply !== undefined) loseReportReply = input.loseReportReply
        }
        return { kind: 'value', value: { pid: process.pid, replacementError, readError, settingsReadError, lookupError: lookupError && { ...lookupError }, reportAttempts } }
      },
    )
    const context: Context = {
      use<T>(name: string): T {
        if (name === 'sessions') return observed as T
        if (name === 'pluginFiles') return observedFiles as T
        if (name === 'sessionPermissions') return observedPermissions as T
        if (name === 'settings') return observedSettings as T
        return ctx.use<T>(name)
      },
      provide: (name, value) => ctx.provide(name, value), effect: cleanup => ctx.effect(cleanup),
      signal: ctx.signal,
      on: (name, listener, options) => ctx.on(name, listener, options),
      filter: keep => ctx.filter(keep),
      emit: (name, ...args) => ctx.emit(name, ...args),
      parallel: (name, ...args) => ctx.parallel(name, ...args),
      serial: (name, ...args) => ctx.serial(name, ...args),
      waterfall: ctx.waterfall,
      bail: (name, ...args) => ctx.bail(name, ...args),
    }
    await scheduler.apply(context, config)
  },
})
