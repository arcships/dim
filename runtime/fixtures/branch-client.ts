import { definePlugin, type PluginSpec } from '@arcships/rutis'
import type { BranchOptions, BranchResult, InputMessageData, TimelineSequence, TreeCommands } from '../../npm/plugin.d.ts'
import declaration from './branch-client.json' with { type: 'json' }

function value<T>(reply: BranchResult<T>): T {
  if (!reply.ok) throw new Error(`${reply.error.code}: ${reply.error.message}`)
  return reply.value
}
type Counter = {
  get(): unknown
  through(request: { through: TimelineSequence }): Promise<unknown>
  arm(): null
  entered(): Promise<null>
  release(): null
  fail(): null
}

export default definePlugin({
  name: 'BranchClientFixture',
  ...declaration,
  provides: declaration.provides as PluginSpec['provides'],
  apply(ctx) {
    const tree = ctx.use<TreeCommands>('treeCommands')
    let retained: Counter | undefined
    ctx.provide('sessionMethods:session.cfive', {
      async describe() {
        return ['get', 'through', 'second', 'list', 'create', 'input', 'retain', 'stale', 'lookup', 'arm', 'entered', 'release', 'fail'].map(name => ({
          name: `session.cfive.${name}`, mutating: true, interruption: 'report', progress: false,
        }))
      },
      async call(call: { method: string; params: { branchId: string; through: TimelineSequence; name: string; data: InputMessageData; options: BranchOptions } }) {
        const action = call.method.slice('session.cfive.'.length)
        const params = call.params
        if (action === 'list') return { kind: 'value', value: tree.list({}) }
        if (action === 'create') {
          const reply = await tree.branch({ parent: params.branchId, at: params.through, options: params.options })
          return { kind: 'value', value: reply.ok ? { ok: true, value: { id: reply.value.id } } : reply }
        }
        if (action === 'stale') {
          try { retained!.get(); return { kind: 'value', value: { failed: false } } }
          catch (error) { return { kind: 'value', value: { failed: true, message: String(error) } } }
        }
        const found = tree.get({ id: params.branchId })
        if (!found.ok) return { kind: 'value', value: found }
        const branch = found.value
        if (action === 'input') return { kind: 'value', value: branch.input(params.data) }
        const service = branch.service<Counter>({ name: action === 'second' ? 'secondCounter' : params.name ?? 'counter' })
        if (action === 'lookup') return { kind: 'value', value: service.ok ? { ok: true } : service }
        const counter = value(service)
        if (action === 'retain') retained = counter
        let result: unknown
        if (action === 'through') result = await counter.through({ through: params.through })
        else if (action === 'arm') result = counter.arm()
        else if (action === 'entered') result = await counter.entered()
        else if (action === 'release') result = counter.release()
        else if (action === 'fail') result = counter.fail()
        else result = counter.get()
        return { kind: 'value', value: result }
      },
    })
  },
})
