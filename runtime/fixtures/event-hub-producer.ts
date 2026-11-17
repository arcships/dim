import { definePlugin } from '@arcships/rutis'
import type { EventHub, EventHubErrorThrown, EventHubPublishRequest } from '../../npm/plugin.js'

const source = 'tsproducer'
const type = 'tsproducer.ping'
const id = 'ts-1'

/**
 * A producer row of another language. The injected `eventHub` name gates the
 * row: it loads only while EventHub provides the service, reloads with it,
 * and declares at every `apply`. The fixed id makes a republish idempotent
 * per branch.
 */
export default definePlugin({
  name: 'EventHubProducerFixture',
  inject: ['eventHub'],
  provides: {},
  async apply(ctx) {
    const hub = ctx.use<EventHub>('eventHub')
    const declare = () => hub.declare({
      source,
      guide: 'The TypeScript fixture source: `tsproducer.ping` says the producer works.',
      types: [{ type, description: 'A ping from the TypeScript fixture', mode: 'queue' }],
    })
    await declare()
    const request: EventHubPublishRequest = { source, type, id, body: 'hello from TypeScript' }
    try {
      await hub.publish(request)
    } catch (error) {
      const failure = error as EventHubErrorThrown
      if (failure.code !== 'unknown_source') throw failure
      await declare()
      await hub.publish(request)
    }
  },
})
