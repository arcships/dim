import { definePlugin } from '@arcships/rutis'
import type { SessionMethods, SubmitChecks } from '../../npm/plugin.js'
import { partRow } from './part-row.ts'

const part = definePlugin({
  name: 'SubmitCheckFixture',
  inject: ['submitChecks', 'sessionMethods'],
  async apply(ctx) {
    const checks = ctx.use<SubmitChecks>('submitChecks')
    await checks.register('fixture', async request => {
      if (request.entry === 'submit') return {kind: 'block', message: 'foreign check refused'}
      return {kind: 'proceed', blocks: [], warnings: ['foreign check ran']}
    })
    await ctx.use<SessionMethods>('sessionMethods').register('session.checkfixture', [
      { name: 'session.checkfixture.probe', mutating: false, interruption: 'report', progress: false },
    ], async (_call, cancelled) => {
      const value = await checks.check({entry: 'branch', branchId: 'main', mode: 'queue'}, () => cancelled)
      return {kind: 'value', value}
    })
  },
})
export default partRow('session', part)
