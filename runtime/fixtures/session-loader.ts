import { definePlugin } from '@arcships/rutis'
import type { SessionMethods, Services } from '../../npm/plugin.js'
import { partRow } from './part-row.ts'

const part = definePlugin({
  name: 'SessionLoaderFixture',
  inject: ['sessionLoader', 'sessionMethods'],
  async apply(ctx) {
    const loader = ctx.use<Services['sessionLoader']>('sessionLoader')
    const namespace = 'session.loaderfixture'
    await ctx.use<SessionMethods>('sessionMethods').register(namespace,
      ['create', 'remove'].map(name => ({
        name: namespace + '.' + name, mutating: true, interruption: 'report', progress: false,
      })),
      async call => {
        if (call.method === namespace + '.create') {
          const { instanceId } = await loader.create({
            group: typeof call.params.group === 'string' ? call.params.group : 'FixtureInstances',
            definitionId: 'fixture-content', instanceData: { body: 'content' },
          })
          return { kind: 'value', value: { instanceId: Number(instanceId) } }
        }
        const id = call.params.instanceId
        if (typeof id !== 'number') throw new Error('missing instanceId')
        await loader.remove({ instanceId: id })
        return { kind: 'value', value: {} }
      })
  },
})

export default partRow('session', part)
