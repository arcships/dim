//! Subscribe this Session to GitHub issue/PR changes.
//!
//! The plugin polls with `gh api`, compares a signature per watched item, and
//! delivers each change back into the Session as an *input* through the
//! `sessions` service. It cannot use events for that: under rutis 0.13 scopes a
//! process-level listener is above the Session and delivery goes upward only,
//! and `timeline.append` writes domain events, not adopted input. A Session row
//! calling `sessions.input` is the supported path in both 0.12 and 0.15.
//!
//! Modes follow the meaning of `InputMessageData` in dim-agent-types:
//! - `steer` applies at the next tool-batch boundary of the running turn (and
//!   wakes an idle branch);
//! - `queue` waits for the current turn to end;
//! - `append` never drives a turn, it only joins the next request built.
import { definePlugin } from '@arcships/rutis'
import { execFile } from 'node:child_process'
import type { Sessions, ToolCall, ToolContribution, ToolResult } from '../../../npm/plugin.d.ts'

export type InputMode = 'queue' | 'steer' | 'append'
export type ChangeKind = 'review' | 'comment' | 'state' | 'metadata'

/** Runs `gh` and returns stdout. Injected by tests. */
export type RunGh = (args: string[]) => Promise<string>

export interface GithubWatchConfig {
  /** Poll interval; default 60 s, minimum 1 s. */
  intervalSeconds?: number
  /** `baseline` records the first poll without delivering it; `report` delivers it. */
  initial?: 'baseline' | 'report'
  /** Per-change mode; review steers, the rest queue, metadata only adds context. */
  modes?: Partial<Record<ChangeKind, InputMode>>
  /** `owner/repo#number` whose comments steer instead of queue. */
  urgent?: string[]
  /** Watched from `apply` once the Session binds; no tool call is needed. */
  targets?: { repo: string; number: number; branchId?: string }[]
  /** Branch for configured targets; the tool uses the calling branch. */
  branchId?: string
}

interface Subscription {
  repo: string
  number: number
  sessionId: string
  branchId: string
  last?: Snapshot
}

interface Snapshot {
  state: string
  title: string
  updated: string
  comments: number
  commentId: number | null
  commentAuthor: string | null
  commentText: string | null
  reviewId: number | null
  reviewState: string | null
  reviewAuthor: string | null
  isPullRequest: boolean
}

const DEFAULT_MODES: Record<ChangeKind, InputMode> = {
  review: 'steer',
  comment: 'queue',
  state: 'queue',
  metadata: 'append',
}
const PRIORITY: InputMode[] = ['append', 'queue', 'steer']

const execGh: RunGh = (args) =>
  new Promise((resolve, reject) => {
    execFile('gh', args, { maxBuffer: 4 * 1024 * 1024 }, (error, stdout) => {
      if (error) reject(error)
      else resolve(stdout)
    })
  })

function key(sub: Subscription) {
  return `${sub.repo}#${sub.number}`
}

function signature(s: Snapshot) {
  return [
    s.updated, s.state, s.title, s.comments, s.commentId, s.reviewId, s.reviewState,
  ].join('|')
}

function highest(modes: InputMode[]) {
  return modes.reduce((acc, mode) => (PRIORITY.indexOf(mode) > PRIORITY.indexOf(acc) ? mode : acc), 'append' as InputMode)
}

function changedKinds(before: Snapshot | undefined, after: Snapshot): ChangeKind[] {
  if (!before) return []
  const kinds: ChangeKind[] = []
  if (before.reviewId !== after.reviewId || before.reviewState !== after.reviewState) kinds.push('review')
  if (before.commentId !== after.commentId || before.comments !== after.comments) kinds.push('comment')
  if (before.state !== after.state) kinds.push('state')
  if (signature(before) !== signature(after) && kinds.length === 0) kinds.push('metadata')
  return kinds
}

function summarize(sub: Subscription, after: Snapshot, kinds: ChangeKind[]) {
  const kind = after.isPullRequest ? 'PR' : 'issue'
  if (kinds.length === 0) {
    return [
      `[github-watch] ${key(sub)} (${kind}) "${after.title}"`,
      `Current state: ${after.state}; ${after.comments} comment(s); review ${after.reviewState ?? 'none'}.`,
      'Read it with `gh issue view` / `gh pr view` when it matters.',
    ].join('\n')
  }
  const parts: string[] = []
  if (kinds.includes('review')) {
    parts.push(`review ${after.reviewState ?? 'unknown'}${after.reviewAuthor ? ` by ${after.reviewAuthor}` : ''}`)
  }
  if (kinds.includes('comment')) {
    const text = after.commentText ? `: "${after.commentText}"` : ''
    parts.push(`comment${after.commentAuthor ? ` by ${after.commentAuthor}` : ''}${text}`)
  }
  if (kinds.includes('state')) parts.push(`state ${after.state}`)
  if (kinds.includes('metadata')) parts.push(`updated ${after.updated}`)
  return [
    `[github-watch] ${key(sub)} (${kind}) "${after.title}"`,
    `Changes: ${parts.join('; ')}.`,
    after.isPullRequest ? 'A review or comment may change what this PR needs.' : '',
    'Read it with `gh issue view` / `gh pr view` when it matters.',
  ].filter(Boolean).join('\n')
}

function submissionId(sub: Subscription, after: Snapshot) {
  return `github-watch:${key(sub)}:${hash(signature(after))}`
}

/** Small stable hash: the submission id only needs to be deterministic. */
function hash(value: string) {
  let h = 2166136261
  for (let i = 0; i < value.length; i++) {
    h ^= value.charCodeAt(i)
    h = Math.imul(h, 16777619)
  }
  return (h >>> 0).toString(36)
}

function validRepo(repo: string) {
  return /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repo)
}

async function snapshot(run: RunGh, sub: Subscription): Promise<Snapshot> {
  const issue = JSON.parse(await run(['api', `repos/${sub.repo}/issues/${sub.number}`]))
  const comments = JSON.parse(
    await run(['api', `repos/${sub.repo}/issues/${sub.number}/comments?per_page=1&sort=created&direction=desc`]),
  )
  const lastComment = Array.isArray(comments) && comments.length > 0 ? comments[comments.length - 1] : null
  const isPullRequest = issue.pull_request !== undefined
  let review: Record<string, unknown> | null = null
  if (isPullRequest) {
    const reviews = JSON.parse(
      await run(['api', `repos/${sub.repo}/pulls/${sub.number}/reviews?per_page=1`]),
    )
    review = Array.isArray(reviews) && reviews.length > 0 ? reviews[reviews.length - 1] : null
  }
  const text = typeof lastComment?.body === 'string' ? lastComment.body.replace(/\s+/g, ' ').slice(0, 160) : null
  return {
    state: String(issue.state ?? 'unknown'),
    title: String(issue.title ?? ''),
    updated: String(issue.updated_at ?? ''),
    comments: Number(issue.comments ?? 0),
    commentId: lastComment?.id ?? null,
    commentAuthor: lastComment?.user?.login ?? null,
    commentText: text,
    reviewId: (review?.id ?? null) as number | null,
    reviewState: (review?.state as string) ?? null,
    reviewAuthor: (review?.user as { login?: string } | undefined)?.login ?? null,
    isPullRequest,
  }
}

/** Builds the plugin. `runGh` is injected by tests; production uses the `gh` CLI. */
export function githubWatch(options: { runGh?: RunGh } = {}) {
  const run = options.runGh ?? execGh
  return definePlugin({
    name: 'GithubWatch',
    inject: ['sessions'],
    provides: {
      'toolContribution:githubWatch': { declaration: 'sync', execute: 'async' },
      'sessionEvents:github-watch': { bind: 'sync' },
    },
    apply(ctx, rawConfig: GithubWatchConfig = {}) {
      const sessions = ctx.use<Sessions>('sessions')
      const modes = { ...DEFAULT_MODES, ...(rawConfig.modes ?? {}) }
      const urgent = new Set(rawConfig.urgent ?? [])
      const subs = new Map<string, Subscription>()
      let closed = false
      let timer: ReturnType<typeof setTimeout> | undefined
      let lastError: string | null = null
      let bound: { sessionId: string; branchId: string } | undefined

      /** Registers one target and records its baseline; throws if unreadable. */
      const watchOne = async (repo: string, number: number, sessionId: string, branchId: string, report: boolean) => {
        const sub: Subscription = { repo, number, sessionId, branchId }
        subs.set(key(sub), sub)
        try {
          const after = await snapshot(run, sub)
          sub.last = after
          if (report) await deliver(sub, after, [])
          return sub
        } catch (error) {
          subs.delete(key(sub))
          throw error
        }
      }

      const modeFor = (sub: Subscription, kinds: ChangeKind[]) =>
        highest(kinds.map((kind) => (kind === 'comment' && urgent.has(key(sub)) ? 'steer' : modes[kind])))

      const deliver = async (sub: Subscription, after: Snapshot, kinds: ChangeKind[]) => {
        const id = submissionId(sub, after)
        const lookup = await sessions.lookup({ sessionId: sub.sessionId, branchId: sub.branchId, submissionId: id })
        if (lookup.ok && lookup.value.status === 'accepted') return false
        const reply = await sessions.input({
          sessionId: sub.sessionId,
          branchId: sub.branchId,
          mode: modeFor(sub, kinds),
          submissionId: id,
          rawBlocks: [{ text: `<dim-github-watch repo="${sub.repo}" number="${sub.number}">` }],
          content: [{ type: 'text', text: summarize(sub, after, kinds) }],
        })
        if (!reply.ok) throw new Error(`${reply.error.code}: ${reply.error.message}`)
        return true
      }

      const pollOne = async (sub: Subscription) => {
        const after = await snapshot(run, sub)
        const kinds = changedKinds(sub.last, after)
        const first = sub.last === undefined
        sub.last = after
        if (first && rawConfig.initial !== 'report') return false
        if (!first && kinds.length === 0) return false
        return deliver(sub, after, kinds)
      }

      const poll = async () => {
        const delivered: string[] = []
        for (const sub of [...subs.values()]) {
          if (closed) return delivered
          try {
            if (await pollOne(sub)) delivered.push(key(sub))
            lastError = null
          } catch (error) {
            lastError = `${key(sub)}: ${error instanceof Error ? error.message : String(error)}`
            console.error('github-watch', lastError)
          }
        }
        return delivered
      }

      const schedule = () => {
        const seconds = Math.max(1, rawConfig.intervalSeconds ?? 60)
        timer = setTimeout(async () => {
          if (closed) return
          await poll()
          if (!closed) schedule()
        }, seconds * 1000)
        timer.unref?.()
      }

      const describe = () =>
        [...subs.values()].map((sub) => ({
          target: key(sub),
          state: sub.last?.state ?? 'unknown',
          title: sub.last?.title ?? '',
          reviews: sub.last?.reviewState ?? null,
          comments: sub.last?.comments ?? 0,
        }))

      const tool: ToolContribution = {
        declaration() {
          return {
            name: 'github_watch',
            description:
              'Subscribe this Session to GitHub issue/PR changes. Changes arrive as input messages: reviews steer, everything else queues. Actions: watch, unwatch, list, poll.',
            source: 'extension',
            risk: 'network',
            inputSchema: {
              type: 'object',
              required: ['action'],
              additionalProperties: false,
              properties: {
                action: { type: 'string', enum: ['watch', 'unwatch', 'list', 'poll'] },
                repo: { type: 'string', description: 'owner/name' },
                number: { type: 'integer', minimum: 1, description: 'issue or PR number' },
                report_initial: { type: 'boolean', description: 'Deliver the current state immediately when watching.' },
              },
            },
          }
        },
        async execute(call: ToolCall): Promise<ToolResult> {
          const args = call.arguments as Record<string, unknown>
          const action = String(args.action ?? '')
          if (closed) return { content: 'github_watch is closing', isError: true }
          if (action === 'list') {
            const items = describe()
            return {
              content: items.length === 0
                ? 'No GitHub subscriptions in this Session.'
                : JSON.stringify(items, null, 2),
              structuredContent: { subscriptions: items, lastError },
            }
          }
          if (action === 'poll') {
            try {
              const delivered = await poll()
              return {
                content: delivered.length === 0
                  ? 'No changes delivered.'
                  : `Delivered changes for: ${delivered.join(', ')}`,
                structuredContent: { delivered, lastError },
              }
            } catch (error) {
              return { content: `github_watch poll failed: ${message(error)}`, isError: true }
            }
          }
          const repo = String(args.repo ?? '')
          const number = Number(args.number ?? 0)
          if (!validRepo(repo) || !Number.isInteger(number) || number < 1) {
            return { content: 'github_watch needs repo "owner/name" and a positive issue/PR number.', isError: true }
          }
          const id = `${repo}#${number}`
          if (action === 'unwatch') {
            const removed = subs.delete(id)
            return {
              content: removed ? `Unwatched ${id}.` : `${id} was not watched.`,
              structuredContent: { removed, subscriptions: describe() },
            }
          }
          if (action !== 'watch') {
            return { content: `Unknown action ${action}.`, isError: true }
          }
          try {
            const sub = await watchOne(repo, number, call.sessionId, call.branchId, args.report_initial === true)
            const after = sub.last
            return {
              content: `Watching ${id} (${after?.isPullRequest ? 'PR' : 'issue'}, ${after?.state}). Polling every ${Math.max(1, rawConfig.intervalSeconds ?? 60)}s; changes arrive as input messages.`,
              structuredContent: { subscriptions: describe() },
            }
          } catch (error) {
            return { content: `github_watch could not read ${id}: ${message(error)}`, isError: true }
          }
        },
      }

      ctx.provide('toolContribution:githubWatch', tool)
      // A Session row cannot see its own Session id, and the tool may not be in
      // the host's tool list, so the configured targets subscribe on bind.
      ctx.provide('sessionEvents:github-watch', {
        bind(binding: { sessionId: string }) {
          bound = { sessionId: binding.sessionId, branchId: rawConfig.branchId ?? 'main' }
          void (async () => {
            for (const target of rawConfig.targets ?? []) {
              if (!validRepo(target.repo) || !Number.isInteger(target.number) || target.number < 1) {
                lastError = `invalid target ${JSON.stringify(target)}`
                console.error('github-watch', lastError)
                continue
              }
              try {
                await watchOne(target.repo, target.number, bound.sessionId, target.branchId ?? bound.branchId, rawConfig.initial === 'report')
                lastError = null
              } catch (error) {
                lastError = `${target.repo}#${target.number}: ${message(error)}`
                console.error('github-watch', lastError)
              }
            }
          })()
          return null
        },
      })
      ctx.effect(() => {
        closed = true
        if (timer !== undefined) clearTimeout(timer)
      })
      schedule()
    },
  })
}

function message(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}

export default githubWatch()
