import { definePlugin, type Plugin } from '@arcships/rutis'
import type {
  AgentEvent, AuxiliaryModel, ModelId, Services, SessionBroadcast, SessionMetadata,
  PartRegistry, SessionMethods, SessionMethodReply, Sessions, Settings, SettingsResult,
} from '../../../npm/plugin.d.ts'
import type { UserInput } from '../../../npm/branch-services.d.ts'
import row from './plugin.json' with { type: 'json' }

type Document = { enabled: boolean; maxLength: number; model: ModelId | null }
type TitleSettings = { read(request: Parameters<Settings['read']>[0]): Promise<SettingsResult<{ document: Document }>> }
// Native event payloads follow their owners' contracts; the SDK envelope leaves them opaque.
type HistoryEvent = Omit<AgentEvent, 'data'> & { data: Record<string, any> }
const SYSTEM = "Write a short title for this conversation: at most 8 words, in the language of the user's message, no quotes, no trailing punctuation. Reply with the title only."
const failure = (code: string, message: string): SessionMethodReply => ({ code, message })
const reloading = () => failure('service_reloading', 'SessionTitle is closing or reloading')

async function prefix(sessions: Sessions, sessionId: string, sequence: number, inputId: string) {
  const inputs = new Map<string, UserInput>()
  let first: UserInput | undefined
  let model: ModelId | undefined
  let operator = false
  let after: number | undefined
  let through: number | undefined
  do {
    const page = await sessions.history({ sessionId, branchId: 'main', after,
      ...(through === undefined ? { atLeast: sequence } : { through }) })
    if (!page.ok) throw new Error('history_failed')
    through = page.value.through
    if (page.value.events.length === 0) break
    const events: HistoryEvent[] = page.value.events
    for (const event of events) {
      if (event.type === 'input' && event.name === 'message' && event.data.user) {
        if (!first) inputs.set(event.id, event.data.user)
        if (event.id === inputId) operator = true
      }
      if (event.type === 'input' && event.name === 'applied' && !first) {
        first = inputs.get(event.data.input_id)
      }
      if (event.type === 'context' && event.name === 'configured') {
        model = { providerId: event.data.settings.providerId, modelId: event.data.settings.modelId }
      }
    }
    after = page.value.events.at(-1)!.sequence
  } while (after < through)
  return { first, model, operator }
}

// The Session part: each Session mounts one copy.
export const part = definePlugin({
  name: 'SessionTitle',
  inject: ['sessions', 'settings', 'sessionMetadata', 'auxiliaryModel', 'sessionIdentity', 'sessionConfig', 'sessionLifecycle', 'sessionMethods'],
  async apply(ctx) {
    const sessions = ctx.use<Sessions>('sessions')
    const settings = ctx.use<TitleSettings>('settings')
    const metadata = ctx.use<SessionMetadata>('sessionMetadata')
    const auxiliary = ctx.use<AuxiliaryModel>('auxiliaryModel')
    const { sessionId } = await ctx.use<Services['sessionIdentity']>('sessionIdentity').get()
    const config = await ctx.use<Services['sessionConfig']>('sessionConfig').get()
    const lifecycle = ctx.use<Services['sessionLifecycle']>('sessionLifecycle')
    const tasks = new Set<Promise<void>>()
    let closed = false
    let generating = false
    let epoch = 0
    let failures = 0
    let noText = false
    let cancel: (() => void) | undefined
    const close = () => { closed = true; cancel?.() }
    ctx.effect(async () => { close(); await Promise.all(tasks) })
    void lifecycle.closing().then(close)

    async function attempt(sequence: number, inputId: string, observedEpoch: number) {
      const started = performance.now()
      const current = () => !closed && observedEpoch === epoch
      let entered = false
      let timer: ReturnType<typeof setTimeout> | undefined
      let cancelled = false
      const diagnostic = (outcome: string) => console.warn('SessionTitle', outcome, Math.round(performance.now() - started))
      try {
        const configured = await settings.read({ source: 'session-title', scope: 'user' })
        if (!current() || (configured.ok && !configured.value.document.enabled)) return
        const stored = await metadata.read({ sessionId: sessionId })
        if (!current() || (stored.ok && stored.value.title)) return
        const snapshot = await prefix(sessions, sessionId, sequence, inputId)
        if (!current() || !snapshot.operator || !snapshot.first) return
        // Starting one operator attempt retires preflight reads from the same cohort.
        epoch++
        entered = true
        generating = true
        const cancellation = new Promise<void>(resolve => {
          cancel = () => { cancelled = true; resolve() }
        })
        timer = setTimeout(() => cancel?.(), 30_000)
        if (!stored.ok) throw new Error('metadata_failed')
        if (!configured.ok) throw new Error('settings_failed')
        const document = configured.value.document
        const text = [...snapshot.first.content.filter(block => block.type === 'text').map(block => block.text).join('\n')].slice(0, 2000).join('')
        if (!text) { noText = true; return }
        const model = document.model ?? snapshot.model
        if (!model) throw new Error('model_unavailable')
        const reply = await auxiliary.complete({ model, request: {
          system: SYSTEM, messages: [{ role: 'user', content: text }],
        } }, () => cancellation)
        if (closed) return
        if (cancelled) throw new Error('timed_out')
        const line = reply.text.trim().split(/[\r\n\u2028\u2029]/, 1)[0].trim()
        const title = [...line.replace(/^\p{Quotation_Mark}+|\p{P}+$/gu, '').replace(/[\s\p{Cc}]+/gu, ' ').trim()].slice(0, document.maxLength).join('').trim()
        if (!title) throw new Error('empty_title')
        const written = await metadata.setTitle({ sessionId: sessionId,
          expectedRevision: stored.value.revision, title: { text: title, source: 'auto' } })
        if (!written.ok && written.error.code === 'storage') throw new Error('storage_failed')
        diagnostic(written.ok ? 'completed' : 'discarded')
      } catch {
        if (!closed) {
          if (entered) failures++
          diagnostic('failed')
        }
      } finally {
        if (entered) {
          clearTimeout(timer)
          cancel = undefined
          generating = false
        }
      }
    }

    // Only an interactive leader Session gets a title.
    if (config.interactive !== false && config.collaborationRole === 'leader') {
      ctx.on(`session/${sessionId}`, (record: SessionBroadcast) => {
        if (closed || record.kind !== 'event' || record.branchId !== 'main') return
        const event = record.event as HistoryEvent
        if (generating || noText || failures >= 3
          || event.type !== 'input' || event.name !== 'applied') return
        const task = attempt(event.sequence, event.data.input_id, epoch)
        tasks.add(task)
        void task.finally(() => tasks.delete(task))
      })
    }

    await ctx.use<SessionMethods>('sessionMethods').register(
      'session.rename',
      [{ name: 'session.rename', mutating: true, interruption: 'report', progress: false }],
      async (call, cancelled) => {
        if (closed) return reloading()
        const value = call.params.title
        if (call.method !== 'session.rename' || (value !== null && typeof value !== 'string')) {
          return failure('invalid_params', 'title must be a string or null')
        }
        const text = value === null ? null : value.trim()
        if (text !== null && (!text || /[\p{Cc}\p{Zl}\p{Zp}]/u.test(value!))) {
          return failure('invalid_params', 'Invalid title or control character')
        }
        let aborted = false
        cancelled.then(() => { aborted = true })
        const configured = await settings.read({ source: 'session-title', scope: 'user' })
        if (closed || aborted) return reloading()
        if (!configured.ok) return failure('storage_failed', 'Cannot read title settings')
        if (text !== null && [...text].length > configured.value.document.maxLength) {
          return failure('invalid_params', 'Invalid title length')
        }
        for (let retry = 0; retry <= 3; retry++) {
          const stored = await metadata.read({ sessionId })
          if (closed || aborted) return reloading()
          if (!stored.ok) return failure(stored.error.code === 'not_found' ? 'session_not_found' : 'storage_failed', 'Cannot read title metadata')
          const written = await metadata.setTitle({ sessionId,
            expectedRevision: stored.value.revision, title: text === null ? null : { text, source: 'manual' } })
          if (written.ok) return { kind: 'value', value: { title: text, titleSource: text === null ? null : 'manual', revision: written.value.revision } }
          if (written.error.code !== 'conflict') return failure(written.error.code === 'not_found' ? 'session_not_found' : 'storage_failed', 'Cannot write title metadata')
          if (closed || aborted) return reloading()
        }
        return failure('service_reloading', 'Title changed during rename')
      },
    )
  },
})

// The plugin's one row registers the Session part (rutis usage §2).
export default definePlugin({
  name: 'SessionTitle', inject: row.inject, provides: row.provides as Plugin['provides'],
  async apply(ctx) {
    await ctx.use<PartRegistry>('partRegistry').registerDefinition('session', 'SessionTitle', part)
  },
})
