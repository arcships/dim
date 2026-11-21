import assert from 'node:assert/strict'
import { writeFile } from 'node:fs/promises'
import { definePlugin } from '@arcships/rutis'
import type { SessionLifecycle } from '../../npm/plugin.js'
import { partRow } from './part-row.ts'

const part = definePlugin<{ receipt: string }>({
  name: 'SessionFaultFixture',
  inject: ['sessionLifecycle'],
  async apply(ctx, config) {
    const lifecycle = ctx.use<SessionLifecycle>('sessionLifecycle')
    assert.equal(await lifecycle.failure({}), null)
    let observed!: () => void
    const delivered = new Promise<void>(resolve => { observed = resolve })
    const subscription = await lifecycle.observeFault(async error => {
      if (error && 'storage' in error && error.storage === 'fixture storage') observed()
    })
    await lifecycle.reportFault({ fault: { component: 'fixture', message: 'first' } })
    await lifecycle.reportFault({ cancelled: 'later' })
    assert.deepEqual(await lifecycle.failure({}), { fault: { component: 'fixture', message: 'first' } })
    await lifecycle.reportFault({ storage: 'fixture storage' })
    await delivered
    await lifecycle.reportFault({ storage: 'later storage' })
    assert.equal(await lifecycle.release(subscription), true)
    assert.equal(await lifecycle.release(subscription), false)
    await writeFile(config.receipt, JSON.stringify(await lifecycle.failure({})))
  },
})

export default partRow('session', part)
