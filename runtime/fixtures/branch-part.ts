import { definePlugin } from '@arcships/rutis'
import type { BranchEvent, BranchServices } from '../../npm/plugin.js'

// A branch part on the exported branch services (branch-typescript.md): it
// rebuilds from committed history, follows live events in order and, after
// the first turn ends, writes one marked input to its own branch with the
// events it has seen. A generation that rebuilt from history reports that
// once, on its first live turn end.
export default definePlugin({
  name: 'BranchPartFixture',
  provides: {},
  inject: ['dispatcher', 'timelineIdentity', 'treeCommands'],
  async apply(ctx) {
    const dispatcher = ctx.use<BranchServices['dispatcher']>('dispatcher')
    const tree = ctx.use<BranchServices['treeCommands']>('treeCommands')
    const { branchId } = await ctx.use<BranchServices['timelineIdentity']>('timelineIdentity').get()
    const seen: string[] = []
    let written = false
    let reported = false
    let replayingMarker = false
    let cutoff = 0n
    let rebuilt!: () => void
    const ready = new Promise<void>(resolve => { rebuilt = resolve })
    // Rebuilding folds only; side effects come from live events.
    const step = (event: BranchEvent, live: boolean) => {
      const key = `${event.type}/${event.name}`
      seen.push(key)
      if (key === 'input/message' && JSON.stringify(event.data).includes('c5-marker')) written = true
      if (key === 'input/message' && JSON.stringify(event.data).includes('c5-rebuilt')) reported = true
      if (live && key === 'turn/end' && !written) {
        written = true
        void tree.input(branchId, {
          blocks: [{ type: 'raw', text: `<c5-marker branch="${branchId}" seen="${seen.join(',')}"/>` }],
          mode: 'queue',
        })
      } else if (live && key === 'turn/end' && replayingMarker && !reported) {
        reported = true
        void tree.input(branchId, {
          blocks: [{ type: 'raw', text: `<c5-rebuilt from="${cutoff}" folded="${seen.length}"/>` }],
          mode: 'queue',
        })
      }
    }
    // Subscribe first, then rebuild through the committed cutoff; live
    // events at or below it are already folded.
    await dispatcher.listen('branch-part-fixture', ['input/message', 'turn/start', 'turn/end'], async event => {
      await ready
      if (BigInt(event.sequence) > cutoff) step(event, true)
    })
    cutoff = BigInt(await dispatcher.committed())
    if (cutoff > 0n) for (const event of await dispatcher.local(0, cutoff)) step(event, false)
    replayingMarker = written
    rebuilt()
  },
})
