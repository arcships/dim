import { appendFileSync } from 'node:fs'
import { definePlugin } from '@arcships/rutis'
import provides from './branch-part.json' with { type: 'json' }

type Reply<T> = { ok: true; value: T } | { ok: false; error: { code: string; message: string } }
type Event = { sequence: number; type: string; name: string; data: any }
type BranchRef = { id: string; input(data: unknown): Reply<Event> }
type TreeCommands = { get(request: { id: string }): Reply<BranchRef> }
type Binding = {
  identity: { sessionId: string; branchId: string }
  dispatcher: { subscribe(keys: string[], handler: (event: Event) => void | Promise<void>): Reply<null> }
  timeline: { committed(): Reply<number>; local(request: { start: number; through: number }): Promise<Reply<Event[]>> }
  activated: Promise<boolean>
  closing: Promise<void>
}
const unwrap = <T>(reply: Reply<T>): T => {
  if (!reply.ok) throw new Error(`${reply.error.code}: ${reply.error.message}`)
  return reply.value
}

// A Goal-shaped branch part (C5): it folds its own branch's local events,
// and after a turn driven by an input asking for it, owned work writes one
// reply input. Its public `echoState` serves the consumed fold.
export default definePlugin({
  name: 'BranchPartFixture',
  provides,
  inject: ['treeCommands'],
  apply(ctx, options: { lifecycleFile?: string; failOn?: string; failBind?: boolean }) {
    const tree = ctx.use<TreeCommands>('treeCommands')
    const generation = crypto.randomUUID()
    let identity: Binding['identity'] | undefined
    let through = 0
    let consumed: number[] = []
    let asked: number | undefined
    const replies = new Set<string>()
    let closed = false
    const work = new Set<Promise<void>>()
    const fold = (event: Event) => {
      if (event.sequence <= through) return
      through = event.sequence
      if (event.type !== 'input' || event.name !== 'message') return
      consumed.push(event.sequence)
      for (const block of event.data.blocks ?? []) if (block.key === 'c5:reply') replies.add(block.text)
      const text = event.data.user?.content?.[0]?.text
      if (typeof text === 'string' && text.startsWith('ask ')) asked = event.sequence
      if (options.failOn && text === options.failOn) throw new Error('fixture handler failure')
    }
    ctx.provide('branchPart:echo', {
      async bind(binding: Binding) {
        if (options.failBind) throw new Error('fixture bind failure')
        identity = binding.identity
        let reconstructed!: () => void
        const ready = new Promise<void>(resolve => { reconstructed = resolve })
        unwrap(binding.dispatcher.subscribe(['input/message', 'turn/end'], async event => {
          await ready
          fold(event)
          if (event.type !== 'turn' || event.name !== 'end' || asked === undefined) return
          const marker = `after:${event.sequence}`
          asked = undefined
          if (replies.has(marker)) return
          replies.add(marker)
          const task = binding.activated.then(active => {
            if (!active || closed) return
            const branch = unwrap(tree.get({ id: identity!.branchId }))
            unwrap(branch.input({
              user: { content: [{ type: 'text', text: `reply ${marker}` }] },
              blocks: [{ type: 'raw', text: marker, key: 'c5:reply' }],
              mode: 'queue',
            }))
          })
          work.add(task)
          task.finally(() => work.delete(task))
        }))
        const cutoff = unwrap(binding.timeline.committed())
        for (const event of unwrap(await binding.timeline.local({ start: 1, through: cutoff }))) fold(event)
        through = Math.max(through, cutoff)
        asked = undefined
        reconstructed()
        binding.closing.then(() => { closed = true })
        return null
      },
    })
    ctx.provide('echoState', {
      get() { return { generation, identity, through, consumed, replies: [...replies].sort() } },
    })
    ctx.provide('echoNotes', { get() { return { notes: identity?.branchId } } })
    ctx.effect(async () => {
      closed = true
      await Promise.allSettled(work)
      if (options.lifecycleFile) appendFileSync(options.lifecycleFile, JSON.stringify({
        generation, branchId: identity?.branchId, pending: work.size,
      }) + '\n')
    })
  },
})
