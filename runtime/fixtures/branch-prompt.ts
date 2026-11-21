import { definePlugin } from '@arcships/rutis'
import type { Prompt, PromptSourceTarget } from '../../npm/branch-services.js'
import type { SessionMethods } from '../../npm/plugin.js'
import { partRow } from './part-row.ts'

const part = definePlugin({
  name: 'BranchPromptFixture',
  inject: ['prompt', 'sessionMethods'],
  async apply(ctx) {
    const prompt = ctx.use<Prompt>('prompt')
    await ctx.use<SessionMethods>('sessionMethods').register('session.promptfixture', [
      { name: 'session.promptfixture.update', mutating: true, interruption: 'report', progress: false },
      { name: 'session.promptfixture.publish', mutating: true, interruption: 'report', progress: false },
    ], async (call) => {
      if (call.method === 'session.promptfixture.publish') {
        const target: PromptSourceTarget = {source: 'fixture-state', scope: {kind: 'branch', timeline: 'main'}}
        const entry = {...target, version: '1', body: 'published content', delivery: 'state', order: 2} as const
        const published = await prompt.configure({sources: [[target, entry]]})
        const unchanged = await prompt.configure({sources: [[target, entry]]})
        return {kind: 'value', value: {published, unchanged}}
      }
      await prompt.initialCompletion({})
      const target: PromptSourceTarget = { source: 'fixture', scope: { kind: 'branch', timeline: 'main' } }
      const outcome = await prompt.configure({ model: { providerId: 'fixture', modelId: 'fixture-model' }, sources: [[target, {
        ...target, version: '1', body: 'frozen fixture content', delivery: 'system', order: 1,
      }]] })
      const model = await prompt.adoptedConfiguration({})
      return { kind: 'value', value: { outcome, modelId: model?.modelId ?? null } }
    })
  },
})
export default partRow('branch', part)
