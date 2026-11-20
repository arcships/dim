import assert from 'node:assert/strict'
import { definePlugin } from '@arcships/rutis'
import type { ManagementMethods, UiExtensions, UiHost } from '../../npm/plugin.js'

export default definePlugin({
  name: 'UiOwnerFixture',
  provides: {},
  inject: ['uiHost', 'uiExtensions', 'managementMethods'],
  async apply(ctx) {
    const host = ctx.use<UiHost>('uiHost')
    const extensions = ctx.use<UiExtensions>('uiExtensions')
    await ctx.use<ManagementMethods>('managementMethods').register('uifixture', [
      { name: 'uifixture.detach', mutating: true, interruption: 'drain', progress: false },
      { name: 'uifixture.release', mutating: true, interruption: 'drain', progress: false },
    ], async call => {
      const connectionId = call.origin?.connectionId
      assert.ok(connectionId)
      const plugin = 'ui-fixture'
      if (call.method === 'uifixture.detach') {
        const params = call.params
        assert.ok(params && typeof params === 'object' && !Array.isArray(params) && 'sessionId' in params)
        const {sessionId} = params
        assert.ok(typeof sessionId === 'string')
        await host.detach({connectionId, sessionId})
        await assert.rejects(extensions.register({plugin, name: plugin, package: {kind: 'unsupported', version: 2}}), /Unsupported UI version 2/)
        assert.ok((await extensions.plugins({})).includes(plugin))
        assert.equal(await extensions.refusal({plugin}), 'Unsupported UI version 2')
      } else {
        await host.release({connectionId})
        await host.finished({connectionId})
        await extensions.withdraw({plugin})
        assert.equal(await extensions.refusal({plugin}), null)
        assert.ok(!(await extensions.plugins({})).includes(plugin))
      }
      return {kind: 'value', value: true}
    })
  },
})
