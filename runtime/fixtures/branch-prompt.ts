import { definePlugin } from '@arcships/rutis'
import type { AdoptedPrompt, ContextPreparation, Prompt, PromptSourceTarget } from '../../npm/branch-services.js'
import type { SessionMethods } from '../../npm/plugin.js'
import { partRow } from './part-row.ts'

const part = definePlugin({
  name: 'BranchPromptFixture',
  inject: ['prompt', 'contextPreparation', 'sessionMethods'],
  async apply(ctx) {
    const prompt = ctx.use<Prompt>('prompt')
    const preparation = ctx.use<ContextPreparation>('contextPreparation')
    await ctx.use<SessionMethods>('sessionMethods').register('session.promptfixture', [
      { name: 'session.promptfixture.update', mutating: true, interruption: 'report', progress: false },
      { name: 'session.promptfixture.publish', mutating: true, interruption: 'report', progress: false },
    ], async (call) => {
      if (call.method === 'session.promptfixture.publish') {
        const adopted = call.params.adopted as AdoptedPrompt
        const base = { adopted, changes: [], inputs: [] }
        const unchanged = await preparation.publish(base)
        const entry = {
          source: 'fixture-state', version: '1', body: 'published content',
          scope: { kind: 'branch', timeline: 'main' }, delivery: 'state', order: 2,
        } as const
        const sequence = await preparation.publish({
          adopted, changes: [{ op: 'set', entry }],
          inputs: [{ type: 'raw', text: entry.body }],
        })
        if (sequence == null) throw new Error('changed configuration produced no batch')
        const published = await preparation.awaitPublished({ sequence })
        return { kind: 'value', value: { published: String(published), unchanged } }
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
