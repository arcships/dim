import { writeFile } from 'node:fs/promises'
import { definePlugin } from '@arcships/rutis'
import type { BranchLifecycle } from '../../npm/plugin.js'
import { partRow } from './part-row.ts'

const opening = definePlugin<{ receipt: string }>({
  name: 'OpeningReceiptFixture',
  inject: ['branchLifecycle'],
  async apply(ctx, config) {
    const lifecycle = ctx.use<BranchLifecycle>('branchLifecycle')
    let start!: () => void
    const started = new Promise<void>(resolve => { start = resolve })
    let stop!: () => void
    const stopped = new Promise<void>(resolve => { stop = resolve })
    let pending: Promise<void> | undefined
    ctx.effect(async () => {
      stop()
      await pending
    })
    await lifecycle.registerOpening(() => {
      pending = stopped.then(() => writeFile(config.receipt, 'settled'))
      start()
      return pending
    })
    await lifecycle.registerOpening(async () => {
      await started
      throw new Error('opening completion refused')
    })
  },
})
export default partRow('branch', opening)
