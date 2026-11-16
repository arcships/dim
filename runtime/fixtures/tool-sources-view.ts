import { appendFileSync } from 'node:fs'
import { definePlugin } from '@arcships/rutis'
import { partRow } from './part-row.ts'
import type { SessionMethods, ToolSourcesState, ToolSourcesView } from '../../npm/plugin.js'

export const part = definePlugin({
  name: 'ToolSourcesViewFixture',
  provides: {},
  inject: ['toolSourcesView', 'sessionMethods'],
  async apply(ctx, config: { audit: string }) {
    const view = ctx.use<ToolSourcesView>('toolSourcesView')
    const snapshots: ToolSourcesState[] = []
    let hold = false
    let entered = false
    let settled = false
    let error: string | undefined
    const observation = view.observe({}, async state => {
      snapshots.push(state)
      if (hold) {
        entered = true
        await new Promise<void>(() => {})
      }
    }).catch(reason => { error = String(reason) }).finally(() => { settled = true })
    ctx.effect(async () => {
      await observation
      appendFileSync(config.audit, JSON.stringify({ entered, settled, error }) + '\n')
    })
    await ctx.use<SessionMethods>('sessionMethods').register(
      'session.toolsources',
      [
        { name: 'session.toolsources.status', mutating: false, interruption: 'report', progress: false },
        { name: 'session.toolsources.hold', mutating: false, interruption: 'report', progress: false },
      ],
      async call => {
        if (call.method === 'session.toolsources.hold') hold = true
        return { kind: 'value', value: { current: view.get({}), snapshots, entered, settled, error } }
      },
    )
  },
})

export default partRow('session', part)
