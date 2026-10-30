import { type Plugin, definePlugin } from '@arcships/rutis'
import { partRow } from '../part-row.ts'
import type { ToolRegistry } from '../../../npm/plugin.js'

type Config = { texts?: { prompt?: string } }

/** The description the row carries when its configuration names none. */
const SHIPPED = 'text-injector fixture: the shipped description'

// A fixture plugin whose one model-visible text is a tool description read
// from its factory configuration (`config.texts.prompt`): a candidate text
// replaces that member of the manifest, and every request carries it.
export const part: Plugin<Config> = definePlugin<Config>({
  name: 'TextInjectorSession',
  inject: ['toolRegistry'],
  provides: {},
  async apply(ctx, config) {
    await ctx.use<ToolRegistry>('toolRegistry').register(
      {
        name: 'fixture_prompt',
        description: config?.texts?.prompt ?? SHIPPED,
        inputSchema: { type: 'object' },
        source: 'extension',
        risk: 'session',
      },
      async () => ({ content: 'ok' }),
    )
  },
})

export default partRow('session', part)
