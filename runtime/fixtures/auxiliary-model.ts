import { definePlugin } from '@arcships/rutis'
import { partRow } from './part-row.ts'
import type { ProviderService, SessionMethods } from '../../npm/plugin.d.ts'
import provides from './auxiliary-model.json' with { type: 'json' }

export const part = definePlugin({
  name: 'AuxiliaryModelFixture',
  inject: ['providerService', 'sessionMethods'],
  provides,
  async apply(ctx) {
    const auxiliary = ctx.use<ProviderService>('providerService')
    await ctx.use<SessionMethods>('sessionMethods').register(
      'session.auxiliary',
      [{ name: 'session.auxiliary.complete', mutating: false, interruption: 'report', progress: false }],
      async (call: { params: { request: Parameters<ProviderService['complete']>[0]; cancellable?: boolean } }, cancelled: Promise<void>) => {
        try {
          const reply = call.params.cancellable
            ? await auxiliary.complete(call.params.request, () => cancelled)
            : await auxiliary.complete(call.params.request)
          return { kind: 'value', value: reply }
        } catch (error) {
          if (!(error instanceof Error) || !('code' in error)) throw error
          return { kind: 'value', value: { error: { code: error.code, message: error.message } } }
        }
      },
    )
  },
})

export default partRow('session', part)
