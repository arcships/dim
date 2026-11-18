import { definePlugin } from '@arcships/rutis'
import { partRow } from './part-row.ts'
import type { BranchEvent, Services } from '../../npm/plugin.js'

export const part = definePlugin({
  name: 'BranchPartFixture',
  provides: {},
  inject: ['dispatcher', 'timeline', 'timelineIdentity', 'treeCommands', 'branchLine'],
  async apply(ctx) {
    const dispatcher = ctx.use<Services['dispatcher']>('dispatcher')
    const timeline = ctx.use<Services['timeline']>('timeline')
    const tree = ctx.use<Services['treeCommands']>('treeCommands')
    const line = ctx.use<Services['branchLine']>('branchLine')
    const lineState = async () => JSON.stringify({
      resolved: await line.resolved(), ending: await line.ending(), cursor: await line.cursor('main'),
    })
    const { branchId } = await ctx.use<Services['timelineIdentity']>('timelineIdentity').get()
    const seen: string[] = []
    let written = false
    let reported = false
    let cutoff = 0n
    let replayingMarker = false
    const step = (event: BranchEvent, live: boolean) => {
      const key = `${event.type}/${event.name}`
      seen.push(key)
      if (key === 'input/message' && JSON.stringify(event.data).includes('c5-marker')) written = true
      if (key === 'input/message' && JSON.stringify(event.data).includes('c5-rebuilt')) reported = true
      if (live && key === 'turn/end' && !written) {
        written = true
        void lineState().then(state => tree.input(branchId, {
          blocks: [{ type: 'raw', text: `<c5-marker branch="${branchId}" seen="${seen.join(',')}"/>${state}` }],
          mode: 'queue',
        }))
      } else if (live && key === 'turn/end' && replayingMarker && !reported) {
        reported = true
        void lineState().then(state => tree.input(branchId, {
          blocks: [{ type: 'raw', text: `<c5-rebuilt from="${cutoff}" folded="${seen.length}"/>${state}` }],
          mode: 'queue',
        }))
      }
    }
    await dispatcher.consume(
      'branch-part-fixture',
      ['input/message', 'turn/start', 'turn/end'],
      async () => {
        cutoff = BigInt(await dispatcher.committed())
        if (cutoff > 0n) {
          const page = await timeline.query({
            scope: 'branch',
            include_publishing: true,
            through: cutoff,
            query: { select: { kind: 'events' } },
            page: { limit: cutoff },
          })
          if (page.result.kind !== 'events') throw new Error('expected event rows')
          for (const { event } of page.result.events) step(event, false)
        }
        replayingMarker = written
        return cutoff
      },
      async event => step(event, true),
    )
  },
})

export default partRow('branch', part)
