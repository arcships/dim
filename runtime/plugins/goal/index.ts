import { type Plugin, definePlugin } from '@arcships/rutis'
import { z } from 'zod'
import type { PartRegistry, Settings } from '../../../npm/plugin.js'
import branch from './branch.ts'
import session from './session.ts'
import { settings } from './settings.ts'
import type { Texts } from './goal.ts'

// The row's factory configuration (A.5.1): Goal's two policy texts. The
// manifest carries the shipped sentences; a candidate replaces them here
// (model-prompts §8.1), so the code keeps no copy of either sentence.
const options = z.strictObject({
  texts: z.strictObject({ objective: z.string().min(1), continuation: z.string().min(1) }),
})

/** A part with the row's texts in place of its own configuration. */
function withTexts<C>(part: Plugin<C>, texts: C) {
  return definePlugin({ ...part, apply: (at, _) => part.apply(at, texts) })
}

// Goal (Goal.md §1) is one row: it registers its Settings source, its
// Session part, which writes change inputs by branch id, and its branch part,
// which folds the branch's events and decides continuation. Each Session and
// branch mounts a copy, which closes over the row's texts.
export default definePlugin({
  name: 'Goal',
  config: z.toJSONSchema(options),
  inject: ['partRegistry', 'settings'],
  provides: {},
  async apply(ctx, config: z.infer<typeof options>) {
    await ctx.use<Settings>('settings').register(settings)
    const parts = ctx.use<PartRegistry>('partRegistry')
    const texts: Texts = config.texts
    await parts.registerDefinition('session', 'GoalSession', withTexts(session, texts))
    await parts.registerDefinition('branch', 'GoalBranch', withTexts(branch, texts))
  },
})
