import assert from 'node:assert/strict'
import { definePlugin } from '@arcships/rutis'
import type { ManagementMethods, ModelId, ManagedProviderService } from '../../npm/plugin.js'

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
    const provider = ctx.use<ManagedProviderService>('providerService')
    await ctx.use<ManagementMethods>('managementMethods').register(
      'hostports',
      [{ name: 'hostports.read', mutating: false, interruption: 'report', progress: false }],
      async (call: { params: { model: ModelId } }) => {
        const pending = await provider.prepareStartup({
          target: { modelId: call.params.model.modelId }, cwd: process.cwd(), preferences: {},
          input: { connection: { apiFormat: 'openai-compatible', baseUrl: 'http://127.0.0.1/v1' }, contextWindow: 16384, maxOutputTokens: 2048 },
        })
        const source = pending.model.metadata?.startup?.connection
        assert.ok(source && pending.admissionId)
        await provider.checkStartup(source)
        await assert.rejects(provider.retainStartup({
          sessionId: 'fixture', generation: 'fixture', model: pending.model, admissionId: 'different',
        }), /does not match/)
        await provider.releaseStartup({ admissionId: pending.admissionId })
        await assert.rejects(provider.checkStartup(source))
        return {
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
        }
      },
    )
  },
})
