import assert from 'node:assert/strict'
import { definePlugin } from '@arcships/rutis'
import type { ManagementMethods, Services, UiExtensions, UiHost } from '../../npm/plugin.js'

export default definePlugin({
  name: 'UiOwnerFixture',
  provides: {},
  inject: ['uiHost', 'uiExtensions', 'managementMethods', 'sessionClientsHost'],
  async apply(ctx) {
    const host = ctx.use<UiHost>('uiHost')
    const clients = ctx.use<Services['sessionClientsHost']>('sessionClientsHost')
    const extensions = ctx.use<UiExtensions>('uiExtensions')
    await ctx.use<ManagementMethods>('managementMethods').register('uifixture', [
      { name: 'uifixture.detach', mutating: true, interruption: 'drain', progress: false },
      { name: 'uifixture.finished', mutating: true, interruption: 'drain', progress: false },
      { name: 'uifixture.release', mutating: true, interruption: 'drain', progress: false },
    ], async call => {
      const connectionId = call.origin?.connectionId
      assert.ok(connectionId)
      const plugin = 'ui-fixture'
      const params = call.params
      assert.ok(params && typeof params === 'object' && !Array.isArray(params) && 'sessionId' in params)
      const {sessionId} = params
      assert.ok(typeof sessionId === 'string')
      if (call.method === 'uifixture.finished') {
        const before = await clients.presences({ sessionId })
        assert.equal(before.presences.length, 1)
        assert.equal(host.finished({connectionId}), undefined)
        const after = await clients.presences({ sessionId })
        assert.equal(after.presences.length, 2)
        assert.ok(after.presences.every(presence => presence.channel !== 'tui'))
      } else if (call.method === 'uifixture.detach') {
        assert.equal(host.detach({connectionId, sessionId}), undefined)
        await assert.rejects(extensions.register({plugin, name: plugin, package: {kind: 'unsupported', version: 2}}), /Unsupported UI version 2/)
        assert.ok((await extensions.plugins({})).includes(plugin))
        assert.equal(await extensions.refusal({plugin}), 'Unsupported UI version 2')
      } else {
        assert.equal(host.release({connectionId}), undefined)
        await extensions.withdraw({plugin})
        assert.equal(await extensions.refusal({plugin}), null)
        assert.ok(!(await extensions.plugins({})).includes(plugin))
      }
      return {kind: 'value', value: true}
    })
  },
})
