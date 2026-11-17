import { definePlugin } from '@arcships/rutis'
import type { ManagementMethods } from '../../npm/plugin.js'

type Read = { read(request: object): Promise<unknown> }
type Model = { providerId: string; modelId: string }

export default definePlugin({
  name: 'HostPortReadsFixture',
  provides: {},
  inject: ['managementMethods', 'executionSettingsHost', 'toolchainSourceHost',
    'promptSuggestionSettingsHost', 'selectableModelsHost', 'modelSelectionHost'],
  async apply(ctx) {
    const execution = ctx.use<Read>('executionSettingsHost')
    const toolchains = ctx.use<{ list(request: object): Promise<unknown> }>('toolchainSourceHost')
    const suggestions = ctx.use<Read>('promptSuggestionSettingsHost')
    const models = ctx.use<{
      selectableModels(request: object): Promise<unknown>
      selectableForModality(request: object): Promise<unknown>
    }>('selectableModelsHost')
    const selection = ctx.use<{
      resolve(request: { explicit: Model }): Promise<unknown>
    }>('modelSelectionHost')
    await ctx.use<ManagementMethods>('managementMethods').register(
      'hostports',
      [{ name: 'hostports.read', mutating: false, interruption: 'report', progress: false }],
      async (call: { params: { model: Model } }) => ({
        kind: 'value',
        value: {
          execution: await execution.read({}),
          toolchains: await toolchains.list({}),
          suggestions: await suggestions.read({}),
          models: await models.selectableModels({}),
          modalities: await models.selectableForModality({}),
          selection: await selection.resolve({ explicit: call.params.model }),
        },
      }),
    )
  },
})
