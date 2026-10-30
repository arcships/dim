import { existsSync } from 'node:fs'
import { definePlugin } from '@arcships/rutis'
import { partRow } from './part-row.ts'
import type { GuideEntries, GuideEntry, SessionMethodReply, SessionMethods } from '../../npm/plugin.js'

type Scope = 'session' | { branch: string }
type Config = { registerFile?: string; marker?: string; text?: string }
type MethodCall = { method: string; params: { sessionId: string; branchId?: string; scope?: Scope; entry?: GuideEntry; [key: string]: unknown } }
type Attempt = { ok: true } | { ok: false; code: string; message: string }

const codeOf = (error: unknown): string =>
  typeof error === 'object' && error !== null && 'code' in error
    ? String((error as { code: unknown }).code)
    : String(error)
const messageOf = (error: unknown): string => (error instanceof Error ? error.message : String(error))

// The entry the fixture registers on its own, for as long as `registerFile`
// exists (Prompt §6.1): a restarted copy whose file is gone registers nothing,
// so the test can show the entry leaving with its contributor.
export const part = definePlugin({
  name: 'GuideEntriesFixture',
  provides: {},
  inject: ['guideEntries', 'sessionMethods'],
  async apply(ctx, config: Config = {}) {
    const entries = ctx.use<GuideEntries>('guideEntries')
    const marker = config.marker ?? 'dim-guide-fixture'
    const text = config.text ?? `<${marker}>: a fixture marker the runtime writes.`
    const attempt = async (scope: Scope, entry: GuideEntry): Promise<Attempt> => {
      try {
        await entries.register(scope, entry)
        return { ok: true }
      } catch (error) {
        return { ok: false, code: codeOf(error), message: messageOf(error) }
      }
    }
    let state: { registered: boolean; refusal?: { code: string; message: string } } = { registered: false }
    if (config.registerFile === undefined || existsSync(config.registerFile)) {
      const outcome = await attempt('session', { markers: [marker], order: 55, text })
      state = outcome.ok ? { registered: true } : { registered: false, refusal: { code: outcome.code, message: outcome.message } }
    }
    await ctx.use<SessionMethods>('sessionMethods').register(
      'session.guide_fixture',
      ['status', 'register'].map(name => ({
        name: `session.guide_fixture.${name}`, mutating: false, interruption: 'report', progress: false,
      })),
      async (call: MethodCall): Promise<SessionMethodReply> => {
        if (call.method.endsWith('.status')) return { kind: 'value', value: state }
        return { kind: 'value', value: await attempt(call.params.scope ?? 'session', call.params.entry ?? { markers: [marker], order: 55, text }) }
      },
    )
  },
})

export default partRow('session', part)
