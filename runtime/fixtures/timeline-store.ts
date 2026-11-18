import { definePlugin } from '@arcships/rutis'
import { partRow } from './part-row.ts'
import type { TimelineStore, TimelineStoreCompletion, SessionMethods, MethodReply } from '../../npm/plugin.js'
import assert from 'node:assert/strict'

export const part = definePlugin({
  name: 'TimelineStoreFixture',
  inject: ['timelineStore', 'sessionMethods'],
  provides: {},
  async apply(ctx) {
    const store = ctx.use<TimelineStore>('timelineStore')
    const completions: TimelineStoreCompletion[] = []
    const releases: (() => void)[] = []
    const subscriptions: string[] = []
    await ctx.use<SessionMethods>('sessionMethods').register(
      'session.timeline_store_fixture',
      ['subscribe', 'status', 'read', 'release', 'close', 'unblock'].map(name => ({
        name: `session.timeline_store_fixture.${name}`, mutating: true, interruption: 'report', progress: false,
      })),
      async (call: { method: string; params: any }): Promise<MethodReply> => {
        const p = call.params
        let value: any = {}
        switch (call.method.split('.').at(-1)) {
          case 'subscribe': {
            const subscriptionId = (await store.commits({ sessionId: p.target }, async completion => {
              completions.push(completion)
              if (p.mode === 'slow') await new Promise<void>(resolve => releases.push(resolve))
              if (p.mode === 'throw') throw new Error('fixture observer failure')
            })).subscriptionId
            subscriptions.push(subscriptionId)
            value = { subscriptionId }
            break
          }
          case 'status': value = { completions }; break
          case 'unblock': releases.splice(0).forEach(release => release()); break
          case 'close':
            for (const subscriptionId of subscriptions.splice(0)) await store.closeCommits({ subscriptionId })
            break
          case 'release': await store.releaseSession({ sessionId: p.target }); break
          case 'read': {
            const key = { sessionId: p.target, branchId: 'main' }
            await assert.rejects(() => Reflect.apply(store.head, store, [[p.target, 'main']]), /request must be an object/)
            await assert.rejects(() => Reflect.apply(store.commits, store, [[p.target], () => {}]), /request must be an object/)
            const { sequence: through } = await store.head(key)
            const { events } = await store.readPage({ ...key, after: 0, limit: 1, through })
            assert.ok(events.length > 0)
            assert.throws(() => store.append({ ...key, batchId: 'foreign-refusal', events }),
              (error: unknown) => error instanceof Error && 'code' in error && error.code === 'invalid'
                && error.message === 'invalid input: Timeline append must run beside its state owner')
            assert.equal(events.at(-1)?.event.sequence, events.at(-1)?.batchEnd)
            const first = events[0]
            const boundary = await store.batchEndAt({ ...key, sequence: first.event.sequence })
            assert.equal(boundary.sequence, first.batchEnd)
            await store.batchEnd({ ...key, sequence: boundary.sequence })
            const segments = await store.readSegments({ sessionId: p.target, segments: [{ branchId: 'main', from: 1, through: boundary.sequence }] })
            assert.deepEqual(segments.segments[0], events)
            const trace = await store.trace({ ...key, sequence: first.event.sequence })
            assert.deepEqual(trace.trace, first.trace)
            const query = await store.query({ scope: {
              key, through, segments: [{ branchId: 'main', from: 1, through, storedThrough: through, tail: [] }],
            }, query: { select: { kind: 'count' } } })
            assert.deepEqual(query, { result: { kind: 'count', count: through }, next: null })
            const last = events[events.length - 1]
            const snapshot = { sequence: last.event.sequence, event_id: last.event.id, interpreter: 'store-fixture', state: '{}' }
            await store.saveSnapshot({ ...key, snapshot })
            assert.deepEqual(await store.latestSnapshot({ ...key, through, interpreter: 'store-fixture' }), { snapshot })
            assert.equal((await store.latestSnapshot({ ...key, through, interpreter: 'other-fixture' })).snapshot, null)
            value = { through, events, branches: await store.branches({ sessionId: p.target }) }
            break
          }
          default: throw new Error('unknown fixture method')
        }
        return { kind: 'value', value }
      },
    )
  },
})
export default partRow('session', part)
