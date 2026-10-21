import { definePlugin } from '@arcships/rutis'
import type { InputWrite, InputReceipt, Sessions, SessionsResult, ManagementMethodsContribution, MethodReply } from '../../npm/plugin.js'

let generation = 0
let cleanup: SessionsResult<InputReceipt> | null = null
export default definePlugin({
  name: 'InputSettlementFixture',
  inject: ['sessions'],
  provides: { 'managementMethods:settlement': { describe: 'async', call: 'async' } },
  apply(ctx) {
    const applied = ++generation
    const sessions = ctx.use<Sessions>('sessions')
    let late: InputWrite | null = null
    ctx.effect(async () => {
      if (late) cleanup = await sessions.input(late)
    })
    const methods: ManagementMethodsContribution = {
      async describe() {
        return ['input', 'arm', 'status'].map(name => ({
          name: `settlement.${name}`, mutating: name !== 'status', interruption: 'drain', progress: false,
        }))
      },
      async call(call): Promise<MethodReply> {
        if (call.method === 'settlement.status') return { kind: 'value', value: JSON.stringify({ generation: applied, pid: process.pid, cleanup }) }
        if (call.method === 'settlement.arm') {
          late = call.params as unknown as InputWrite
          return { kind: 'value', value: true }
        }
        return { kind: 'value', value: JSON.stringify(await sessions.input(call.params as unknown as InputWrite)) }
      },
    }
    ctx.provide('managementMethods:settlement', methods)
  },
})
