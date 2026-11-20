import { definePlugin } from '@arcships/rutis'
import type { ManagementMethods, ModelId, ProviderService } from '../../npm/plugin.js'

type Read = { read(request: object): Promise<unknown> }

export default definePlugin({
  name: 'HostPortReadsFixture',
  provides: {},
  inject: ['managementMethods', 'executionSettingsHost', 'toolchainSourceHost',
    'promptSuggestionSettingsHost', 'providerService'],
  async apply(ctx) {
    const execution = ctx.use<Read>('executionSettingsHost')
    const toolchains = ctx.use<{ list(request: object): Promise<unknown> }>('toolchainSourceHost')
    const suggestions = ctx.use<Read>('promptSuggestionSettingsHost')
    const provider = ctx.use<ProviderService>('providerService')
    await ctx.use<ManagementMethods>('managementMethods').register(
      'hostports',
      [{ name: 'hostports.read', mutating: false, interruption: 'report', progress: false }],
      async (call: { params: { model: ModelId } }) => ({
        kind: 'value',
        value: {
          execution: await execution.read({}),
          toolchains: await toolchains.list({}),
          suggestions: await suggestions.read({}),
          models: await provider.selectableModels({}),
          modalities: await provider.selectableForModality({}),
          adopted: await provider.applyPreferences({ current: await provider.configuration({ explicit: call.params.model, preferences: {} }), update: {} }),
          selection: await provider.resolve({ explicit: call.params.model }),
        },
      }),
    )
  },
})
