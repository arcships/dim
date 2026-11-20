import { definePlugin } from '@arcships/rutis'
import { partRow } from './part-row.ts'
import type { BranchEvent, Services } from '../../npm/plugin.js'

export const part = definePlugin({
  name: 'ExtensionEventsFixture',
  provides: {},
  inject: ['dispatcher', 'timeline', 'timelineIdentity', 'timelineTree'],
  async apply(ctx) {
    const dispatcher = ctx.use<Services['dispatcher']>('dispatcher')
    const timeline = ctx.use<Services['timeline']>('timeline')
    const tree = ctx.use<Services['timelineTree']>('timelineTree')
    const { branchId } = await ctx.use<Services['timelineIdentity']>('timelineIdentity').get()
    let appended = false
    let restored = false
    let reported = false
    const attempt = async (call: () => Promise<unknown>) => {
      try {
        await call()
        return 'accepted'
      } catch (error) {
        return `rejected: ${(error as Error).message}`
      }
    }
    const step = async (event: BranchEvent, live: boolean) => {
      const key = `${event.type}/${event.name}`
      if (key === 'fixture/turn-seen') {
        appended = true
        if (!live) restored = true
        if (live) {
          await tree.input(branchId, {
            blocks: [{ type: 'raw', text: `<ext-seen key="${key}" data=${JSON.stringify(JSON.stringify(event.data))}/>` }],
            mode: 'queue',
          })
        }
      } else if (live && key === 'turn/end' && !appended) {
        appended = true
        const malformed = await attempt(() => timeline.append({ type: 'input', name: 'forged', data: {} }))
        const close = await attempt(() => (timeline as unknown as { close(): Promise<void> }).close())
        await timeline.append({ type: 'fixture', name: 'turn-seen', data: { turn: event.data, malformed, close } })
      } else if (live && key === 'turn/end' && restored && !reported) {
        reported = true
        await tree.input(branchId, {
          blocks: [{ type: 'raw', text: `<ext-rebuilt appended="${appended}"/>` }],
          mode: 'append',
        })
      }
    }
    await dispatcher.consume(
      'extension-events-fixture',
      ['turn/end', 'fixture/*'],
      async () => {
        const through = BigInt(await dispatcher.committed())
        if (through > 0n) {
          const page = await timeline.query({
            scope: 'branch',
            include_publishing: true,
            through: through,
            query: { select: { kind: 'events' } },
            page: { limit: through },
          })
          if (page.result.kind !== 'events') throw new Error('expected event rows')
          for (const { event } of page.result.events) await step(event, false)
        }
        return through
      },
      async event => step(event, true),
    )
  },
})

export default partRow('branch', part)
