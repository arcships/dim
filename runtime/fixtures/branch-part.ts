import { appendFileSync } from 'node:fs'
import { definePlugin, type PluginSpec } from '@arcships/rutis'
import type { AgentEvent, BranchBinding, BranchResult, TimelineSequence, TreeCommands } from '../../npm/plugin.d.ts'
import declaration from './branch-part.json' with { type: 'json' }

function value<T>(reply: BranchResult<T>): T {
  if (!reply.ok) throw new Error(`${reply.error.code}: ${reply.error.message}`)
  return reply.value
}

export default definePlugin({
  name: 'BranchCounterFixture',
  ...declaration,
  provides: declaration.provides as PluginSpec['provides'],
  apply(ctx, options: { lifecycleFile: string }) {
    const tree = ctx.use<TreeCommands>('treeCommands')
    const generation = crypto.randomUUID()
    const events: AgentEvent[] = []
    const tasks: Promise<void>[] = []
    const changed = new Set<() => void>()
    let binding: BranchBinding
    let through: TimelineSequence = 0
    let reconstructed = Promise.resolve()
    let closed = false
    let active = false
    let settled = false
    let hold = false
    let entered = false
    let throwing = false
    let release: (() => void) | undefined
    let taskError: string | undefined
    const notify = () => { for (const wake of changed) wake() }
    const until = async (ready: () => boolean) => {
      while (!ready()) await new Promise<void>(resolve => {
        const wake = () => { changed.delete(wake); resolve() }
        changed.add(wake)
      })
    }
    const fold = (event: AgentEvent) => {
      if (event.sequence <= through) return
      events.push(event)
      through = event.sequence
      notify()
    }
    const get = () => ({ generation, identity: binding.identity, through, events, active, taskError })
    ctx.effect(async () => {
      closed = true
      release?.()
      notify()
      await Promise.all(tasks)
      appendFileSync(options.lifecycleFile, JSON.stringify({ generation, closed, active, settled, count: events.length }) + '\n')
    })
    ctx.provide('branchPart:counter', {
      async bind(reference: BranchBinding) {
        binding = reference
        binding.closing.then(() => { closed = true; release?.(); notify() })
        tasks.push((async () => {
          active = await binding.activated
          await binding.closing
          settled = true
        })())
        const subscribed = binding.dispatcher.subscribe(['*'], async event => {
          await reconstructed
          if (event.sequence <= through || closed) return
          if (hold && event.type === 'input' && event.name === 'message') {
            entered = true; notify()
            await new Promise<void>(resolve => { release = resolve })
          }
          if (closed) return
          if (throwing) throw new Error('fixture critical callback failure')
          fold(event)
          if (event.type === 'input' && event.name === 'message' && event.data.submission_id === 'emit') {
            // Owned work waits for activation outside the Dispatcher callback.
            tasks.push((async () => {
              if (!await binding.activated || closed) return
              const target = value(tree.get({ id: binding.identity.branchId }))
              value(target.input({ submission_id: 'emitted', user: { content: [{ type: 'text', text: 'branch-owned input' }] } }))
            })().catch(error => { taskError = String(error) }))
          }
        })
        value(subscribed)
        const cutoff = value(binding.timeline.committed())
        reconstructed = (async () => {
          for (const event of value(await binding.timeline.local({ start: 1, through: cutoff }))) fold(event)
        })()
        await reconstructed
        return null
      },
    })
    ctx.provide('counter', {
      get,
      async through(request: { through: TimelineSequence }) {
        await until(() => closed || through >= request.through)
        if (closed) throw new Error('provider unloaded')
        return get()
      },
      arm() { hold = true; entered = false; return null },
      async entered() { await until(() => entered || closed); return null },
      release() { hold = false; release?.(); return null },
      fail() { throwing = true; return null },
    })
    ctx.provide('secondCounter', { get: () => ({ generation, identity: binding.identity }) })
  },
})
