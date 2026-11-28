import { definePlugin } from '@arcships/rutis'
import type { InputMessageData } from '../../npm/branch-services.js'
import type { ManagementMethods, MethodReply, Sessions } from '../../npm/plugin.js'

/** One input the fixture writes: the line it targets, and the write itself. */
type Write = InputMessageData & { sessionId: string; branchId: string }

let generation = 0
let cleanup: unknown = null
const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms))

export default definePlugin({
  name: 'InputSettlementFixture',
  inject: ['sessions', 'managementMethods'],
  provides: {},
  async apply(ctx) {
    const applied = ++generation
    const sessions = ctx.use<Sessions>('sessions')
    // The write answers its commit and the publication is a second operation
    // on the same line: a caller that must not outrun the store waits for it,
    // retrying while the answer says it may.
    const settle = async (request: Write) => {
      const written = await sessions.call({ sessionId: request.sessionId, lineId: request.branchId,
        service: 'timelineContext', method: 'input', args: [request] })
      if (!written.ok) return written
      const event = written.value.event
      for (;;) {
        const published = await sessions.call({ sessionId: request.sessionId, lineId: request.branchId,
          service: 'timelineContext', method: 'waitPublished', args: [event.sequence] })
        if (published.ok) return { ok: true as const, value: event }
        if (published.error.retryable !== true) return published
        await sleep(10)
      }
    }
    let late: Write | null = null
    ctx.effect(async () => {
      if (late) cleanup = await settle(late)
    })
    await ctx.use<ManagementMethods>('managementMethods').register(
      'settlement',
      ['input', 'arm', 'status'].map(name => ({
        name: `settlement.${name}`, mutating: name !== 'status', interruption: 'drain', progress: false,
      })),
      async (call): Promise<MethodReply> => {
        if (call.method === 'settlement.status') return { kind: 'value', value: JSON.stringify({ generation: applied, pid: process.pid, cleanup }) }
        if (call.method === 'settlement.arm') {
          late = call.params as unknown as Write
          return { kind: 'value', value: true }
        }
        return { kind: 'value', value: JSON.stringify(await settle(call.params as unknown as Write)) }
      },
    )
  },
})
