import { definePlugin } from '@arcships/rutis'
import { partRow } from './part-row.ts'
import type { Services, SessionMethods, SkillsBranch } from '../../npm/plugin.js'

export const part = definePlugin({
  name: 'SkillsListFixture',
  provides: {},
  inject: ['skillsBranch', 'sessionMethods', 'timelineIdentity'],
  async apply(ctx) {
    const skills = ctx.use<SkillsBranch>('skillsBranch')
    const { branchId } = await ctx.use<Services['timelineIdentity']>('timelineIdentity').get()
    const namespace = `session.skillface.${branchId}`
    await ctx.use<SessionMethods>('sessionMethods').register(
      namespace,
      [{ name: `${namespace}.list`, mutating: false, interruption: 'report', progress: false }],
      async () => ({ kind: 'value', value: { skills: skills.list({}) } }),
    )
  },
})

export default partRow('branch', part)
