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
    ], async () => {
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
