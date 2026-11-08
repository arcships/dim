import { definePlugin } from '@arcships/rutis'
import { partRow } from './part-row.ts'
import provides from './session-part.json' with { type: 'json' }
import type { CallChannel, SessionMethods, ToolRegistry } from '../../npm/plugin.js'

type Call = {
  sessionId: string; branchId: string; turnId: string; callId: string
  arguments: { text: string }; config: { cwd: string }
}
type MethodCall = { callId: string; method: string; params: { sessionId: string }; origin?: { connectionId: string } }

const channelClosed: string[] = []
let lastChannelCall: { connectionId: string; callId: string } | undefined
async function codeOf(request: () => Promise<unknown>): Promise<string | null> {
  try {
    await request()
    return null
  } catch (error) {
    return (error as { code?: string }).code ?? null
  }
}

export const part = definePlugin({
  name: 'SessionTypeScriptFixture',
  inject: ['toolRegistry', 'sessionMethods', 'callChannel'],
  provides,
  async apply(ctx, config: { badDeclaration?: boolean; badMethods?: boolean | 'unsupported'; processOwner?: boolean; namespace?: string; typeArray?: boolean; badTypeArray?: boolean }) {
    const generation = crypto.randomUUID()
    const owner = config.processOwner ? ctx.use<{ status(): { generation: number } }>('exampleProcess') : undefined
    let stopped = false
    let calls = 0
    let fastFinished = false
    let finishFast: (() => void) | undefined
    let toolStarted = false
    let startTool: (() => void) | undefined
    let bStarted = false
    let startB: (() => void) | undefined
    let holdA: (() => void) | undefined
    const alive = () => { if (stopped) throw new Error('provider unloaded') }
    const pending = new Set<Promise<void>>()
    ctx.effect(async () => {
      await Promise.all(pending)
      stopped = true
    })
    let refused: string | undefined
    await ctx.use<ToolRegistry>('toolRegistry').register(
      {
        name: 'ts_echo', description: 'TypeScript flow fixture', inputSchema: { type: 'object' },
        source: 'extension', ...(config.badDeclaration ? {} : { risk: 'session' }),
      },
      async (call: Call, cancelled: Promise<void>) => {
        alive()
        calls++
        if (call.arguments.text === 'malformed') return { content: { invalid: true } }
        if (call.arguments.text === 'declares') {
          return {
            content: 'declared',
            effects: [
              { kind: 'fileWritten', path: '/work/fixture.rs' },
              { kind: 'commandRun', command: 'npm test', exitCode: 0 },
            ],
          }
        }
        if (call.arguments.text === 'fast') {
          cancelled.then(() => { fastFinished = true; finishFast?.() })
        }
        if (call.arguments.text === 'cancel') {
          toolStarted = true; startTool?.()
          await cancelled
        }
        return { content: JSON.stringify({ ...call, generation, calls, processGeneration: owner?.status().generation }) }
      },
    ).catch((error: Error) => { refused = error.message })
    if (config.typeArray) {
      await ctx.use<ToolRegistry>('toolRegistry').register(
        {
          name: 'ts_nullable', description: 'TypeScript flow fixture',
          inputSchema: { type: 'object', properties: { space_id: { type: ['string', 'null'] } }, required: ['space_id'] },
          source: 'extension', risk: 'session',
        },
        async () => ({ content: 'fixture' }),
      ).catch((error: Error) => { refused = error.message })
    }
    if (config.badTypeArray) {
      await ctx.use<ToolRegistry>('toolRegistry').register(
        {
          name: 'ts_broken', description: 'TypeScript flow fixture',
          inputSchema: { type: 'object', properties: { x: { type: ['string', 'file'] } } },
          source: 'extension', risk: 'session',
        },
        async () => ({ content: 'fixture' }),
      ).catch((error: Error) => { refused = error.message })
    }
    const namespace = config.namespace ?? 'session.typescript'
    const callChannel = ctx.use<CallChannel>('callChannel')
    const declarations = config.badMethods === 'unsupported'
      ? [{ name: 'session.typescript.bad', mutating: false, interruption: 'report', progress: false, reply: 'direct' }]
      : config.badMethods
        ? [{ name: 'session.typescript.bad', mutating: false }]
        : ['status', 'wait_fast', 'wait_tool', 'error', 'malformed', 'undefined', 'extra_value', 'extra_error', 'handed_off', 'report', 'drain', 'channel', 'channel_state', 'channel_after_b', 'channel_hold_after_b', 'channel_block', 'channel_release', 'channel_bad_request', 'channel_unknown', 'channel_too_large', 'channel_after_reply', 'channel_notify_after_reply', 'channel_remember', 'channel_after_close_codes'].map(name => ({
          name: `${namespace}.${name}`, mutating: false,
          interruption: name === 'drain' ? 'drain' : 'report', progress: true,
          notifications: name === 'channel' ? [`${namespace}.changed`] : [],
        }))
    await ctx.use<SessionMethods>('sessionMethods').register(
      namespace, declarations,
      async (call: MethodCall, cancelled: Promise<void>, progress: (data: unknown) => void) => {
        let done!: () => void
        const settled = new Promise<void>(resolve => { done = resolve })
        pending.add(settled)
        try {
          alive()
          if (call.method.endsWith('.error')) return { code: `owned_${generation}`, message: 'fixture error' }
          if (call.method.endsWith('.undefined')) return { kind: 'value', value: undefined }
          if (call.method.endsWith('.extra_value')) return { kind: 'value', value: null, extra: true }
          if (call.method.endsWith('.extra_error')) return { code: 'fixture', message: 'fixture error', extra: true }
          if (call.method.endsWith('.handed_off')) return { kind: 'handed_off' }
          if (call.method.endsWith('.malformed')) return { invalid: true }
          if (call.method.endsWith('.channel')) {
            bStarted = true
            startB?.()
            const connectionId = call.origin?.connectionId
            if (!connectionId) return { kind: 'value', value: { error: 'no origin' } }
            let code: string | null = null
            try {
              await callChannel.notify({ connectionId, callId: call.callId, method: `${namespace}.notdeclared`, params: {} })
            } catch (error) {
              code = (error as { code?: string }).code ?? null
            }
            void callChannel.closed({ connectionId }).then(() => { channelClosed.push(connectionId) })
            await callChannel.notify({ connectionId, callId: call.callId, method: `${namespace}.changed`, params: { ok: true } })
            return { kind: 'value', value: { code, connectionId } }
          }
          if (call.method.endsWith('.channel_after_b') || call.method.endsWith('.channel_hold_after_b')) {
            const connectionId = call.origin?.connectionId
            if (!connectionId) return { kind: 'value', value: { error: 'no origin' } }
            if (!bStarted) await new Promise<void>(resolve => { startB = resolve })
            await callChannel.notify({ connectionId, callId: call.callId, method: `${namespace}.changed`, params: { call: 'A' } })
            if (call.method.endsWith('.channel_hold_after_b')) await new Promise<void>(resolve => { holdA = resolve })
            return { kind: 'value', value: { connectionId, callId: call.callId } }
          }
          if (call.method.endsWith('.channel_block')) {
            bStarted = true
            startB?.()
            await new Promise<void>(resolve => { holdA = resolve })
            return { kind: 'value', value: {} }
          }
          if (call.method.endsWith('.channel_release')) {
            holdA?.()
            return { kind: 'value', value: {} }
          }
          if (call.method.endsWith('.channel_bad_request')) {
            const connectionId = call.origin?.connectionId
            if (!connectionId) return { kind: 'value', value: { error: 'no origin' } }
            try {
              const notify = callChannel.notify as unknown as (request: unknown) => Promise<unknown>
              await notify({ connectionId, method: `${namespace}.changed`, params: {} })
              return { kind: 'value', value: { code: null } }
            } catch (error) {
              const failure = error as { code?: string; message?: string }
              return { kind: 'value', value: { code: failure.code ?? null, message: String(failure.message ?? error) } }
            }
          }
          if (call.method.endsWith('.channel_unknown')) {
            try {
              await callChannel.notify({ connectionId: 'unknown-connection', callId: call.callId, method: `${namespace}.changed`, params: {} })
              return { kind: 'value', value: { code: null } }
            } catch (error) {
              return { kind: 'value', value: { code: (error as { code?: string }).code ?? null } }
            }
          }
          if (call.method.endsWith('.channel_too_large')) {
            const connectionId = call.origin?.connectionId
            if (!connectionId) return { kind: 'value', value: { error: 'no origin' } }
            try {
              await callChannel.notify({ connectionId, callId: call.callId, method: `${namespace}.changed`, params: { text: 'x'.repeat(16 * 1024 * 1024) } })
              return { kind: 'value', value: { code: null } }
            } catch (error) {
              const failure = error as { code?: string; message?: string }
              return { kind: 'value', value: { code: failure.code ?? null, message: String(failure.message ?? error) } }
            }
          }
          if (call.method.endsWith('.channel_after_reply')) {
            const connectionId = call.origin?.connectionId
            if (!connectionId) return { kind: 'value', value: { error: 'no origin' } }
            lastChannelCall = { connectionId, callId: call.callId }
            return { kind: 'value', value: { connectionId } }
          }
          if (call.method.endsWith('.channel_notify_after_reply')) {
            if (!lastChannelCall) return { kind: 'value', value: { error: 'no replied call' } }
            await callChannel.notify({ ...lastChannelCall, method: `${namespace}.changed`, params: { after: 'reply' } })
            return { kind: 'value', value: {} }
          }
          if (call.method.endsWith('.channel_remember')) {
            const connectionId = call.origin?.connectionId
            if (!connectionId) return { kind: 'value', value: { error: 'no origin' } }
            lastChannelCall = { connectionId, callId: call.callId }
            void callChannel.closed({ connectionId }).then(() => { channelClosed.push(connectionId) })
            return { kind: 'value', value: { connectionId } }
          }
          if (call.method.endsWith('.channel_after_close_codes')) {
            if (!lastChannelCall) return { kind: 'value', value: { error: 'no remembered call' } }
            return { kind: 'value', value: {
              undeclared: await codeOf(() => callChannel.notify({ ...lastChannelCall!, method: `${namespace}.notdeclared`, params: {} })),
              declared: await codeOf(() => callChannel.notify({ ...lastChannelCall!, method: `${namespace}.changed`, params: {} })),
            } }
          }
          if (call.method.endsWith('.channel_state')) {
            return { kind: 'value', value: { closed: channelClosed.length } }
          }
          if (call.method.endsWith('.wait_fast') && !fastFinished) await new Promise<void>(resolve => { finishFast = resolve })
          if (call.method.endsWith('.wait_tool') && !toolStarted) await new Promise<void>(resolve => { startTool = resolve })
          const hold = call.method.endsWith('.report') || call.method.endsWith('.drain')
          progress({ phase: 'started', generation })
          if (hold) await cancelled
          alive()
          progress({ phase: 'settled', generation })
          return { kind: 'value', value: { sessionId: call.params.sessionId, generation, pid: process.pid, calls, cancelled: hold, alive: !stopped, refused } }
        } finally {
          pending.delete(settled)
          done()
        }
      },
    )
  },
})

export default partRow('session', part)
