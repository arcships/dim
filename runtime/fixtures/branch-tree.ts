import { definePlugin } from '@arcships/rutis'
import provides from './branch-tree.json' with { type: 'json' }

type Reply<T> = { ok: true; value: T } | { ok: false; error: { code: string; message: string } }
type Service = Record<string, (request: unknown) => unknown>
type BranchRef = { id: string; input(data: unknown): Reply<{ sequence: number }>; service(request: { name: string }): Reply<Service> }
type TreeCommands = {
  get(request: { id: string }): Reply<BranchRef>
  list(request: Record<string, never>): Reply<unknown[]>
  branch(request: { parent: string; at: number; options: unknown }): Promise<Reply<BranchRef>>
}
type MethodCall = { method: string; params: { sessionId: string; branchId?: string; [key: string]: any } }

// A Session part (C5) that reaches branches through treeCommands: it writes
// branch input and reads a branch part's public service from its owner.
export default definePlugin({
  name: 'BranchTreeFixture',
  provides,
  inject: ['treeCommands'],
  apply(ctx) {
    const tree = ctx.use<TreeCommands>('treeCommands')
    const value = (value: unknown) => ({ kind: 'value', value })
    ctx.provide('sessionMethods:session.tree', {
      async describe() {
        return ['write', 'read', 'list', 'branch'].map(name => ({
          name: `session.tree.${name}`, mutating: false, interruption: 'report', progress: false,
        }))
      },
      async call(call: MethodCall) {
        const action = call.method.slice('session.tree.'.length)
        const params = call.params
        if (action === 'list') return value(tree.list({}))
        if (action === 'branch') {
          const created = await tree.branch({ parent: params.parent, at: params.at, options: {
            prompt: 'child', source: { kind: 'host', operation_id: crypto.randomUUID() },
          } })
          return value(created.ok ? { id: created.value.id } : created)
        }
        const branch = tree.get({ id: params.branchId ?? 'main' })
        if (!branch.ok) return value(branch)
        if (action === 'write') {
          const written = branch.value.input({ user: { content: [{ type: 'text', text: params.text }] }, mode: 'queue' })
          return value(written.ok ? { sequence: written.value.sequence } : written)
        }
        const service = branch.value.service({ name: params.name })
        if (!service.ok) return value(service)
        return value({ methods: Object.keys(service.value).sort(), state: service.value.get({}) })
      },
    })
  },
})
