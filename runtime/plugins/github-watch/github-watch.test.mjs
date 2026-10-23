import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'
import { load } from '@arcships/rutis/testing'
import { githubWatch } from './index.ts'

/** Fake `sessions` service: records inputs, remembers accepted submission ids. */
function fakeSessions() {
  const inputs = []
  const accepted = new Set()
  return {
    inputs,
    service: {
      async lookup(request) {
        return {
          ok: true,
          value: accepted.has(request.submissionId)
            ? { status: 'accepted', inputId: 'prior', sequence: 1 }
            : { status: 'unknown' },
        }
      },
      async input(request) {
        inputs.push(request)
        accepted.add(request.submissionId)
        return { ok: true, value: { inputId: `input:${inputs.length}`, sequence: inputs.length } }
      },
    },
  }
}

/** Fake `gh`: answers the three API shapes the plugin reads. */
function fakeGh(state) {
  return async (args) => {
    state.ghCalls += 1
    const path = args[1] ?? ''
    if (path.includes('/comments?')) return JSON.stringify(state.comment ? [state.comment] : [])
    if (path.includes('/reviews?')) return JSON.stringify(state.reviews ?? [])
    if (path.startsWith('repos/')) {
      return JSON.stringify({
        state: state.state ?? 'open',
        title: state.title ?? 'upgrade design',
        updated_at: state.updated ?? 't0',
        comments: state.comments ?? 0,
        ...(state.pr ? { pull_request: {} } : {}),
      })
    }
    throw new Error(`unexpected gh ${args.join(' ')}`)
  }
}

const toolCall = (args) => ({
  sessionId: 's1',
  branchId: 'main',
  turnId: 't1',
  callId: 'c1',
  arguments: args,
  config: {
    cwd: '/workspace',
    collaborationRole: 'leader',
    permissionPolicy: { preset: 'workspace-write', allowedTools: [], disallowedTools: [] },
    model: { providerId: 'test', modelId: 'test' },
    systemPrompt: '',
    tools: ['github_watch'],
  },
})

async function open(config = {}, state = { ghCalls: 0 }) {
  const sessions = fakeSessions()
  const plugin = githubWatch({ runGh: fakeGh(state) })
  const loaded = await load(plugin, { config, services: { sessions: sessions.service } })
  return { loaded, sessions, state, tool: loaded.service('toolContribution:githubWatch') }
}

const execute = (tool, args) => tool.execute(toolCall(args), new Promise(() => {}))

test('watch records the baseline and later polls deliver one queued comment', async (t) => {
  const { loaded, sessions, state, tool } = await open()
  t.after(() => loaded.unload())

  const watched = await execute(tool, { action: 'watch', repo: 'arcships/dim', number: 361 })
  assert.equal(watched.isError, undefined)
  assert.match(watched.content, /Watching arcships\/dim#361/)
  assert.equal(sessions.inputs.length, 0)

  state.comment = { id: 10, user: { login: 'alice' }, body: 'please rebase' }
  state.comments = 1
  const polled = await execute(tool, { action: 'poll' })
  assert.match(polled.content, /Delivered changes for: arcships\/dim#361/)
  assert.equal(sessions.inputs.length, 1)
  const input = sessions.inputs[0]
  assert.equal(input.mode, 'queue')
  assert.match(input.submissionId, /^github-watch:arcships\/dim#361:/)
  assert.match(input.rawBlocks[0].text, /<dim-github-watch repo="arcships\/dim" number="361">/)
  assert.match(input.content[0].text, /comment by alice: "please rebase"/)

  const again = await execute(tool, { action: 'poll' })
  assert.equal(again.content, 'No changes delivered.')
  assert.equal(sessions.inputs.length, 1)
})

test('report_initial delivers the current state as context, not as a turn', async (t) => {
  const { loaded, sessions, tool } = await open()
  t.after(() => loaded.unload())
  await execute(tool, { action: 'watch', repo: 'arcships/dim', number: 361, report_initial: true })
  assert.equal(sessions.inputs.length, 1)
  assert.equal(sessions.inputs[0].mode, 'append')
  assert.match(sessions.inputs[0].content[0].text, /Current state: open/)
})

test('a review steers, and a review plus a comment in one poll is one steer', async (t) => {
  const { loaded, sessions, state, tool } = await open({}, { ghCalls: 0, pr: true })
  t.after(() => loaded.unload())

  await execute(tool, { action: 'watch', repo: 'arcships/dim', number: 361 })
  state.reviews = [{ id: 5, state: 'changes_requested', user: { login: 'zerob13' } }]
  state.comment = { id: 11, user: { login: 'alice' }, body: 'and this' }
  state.comments = 2

  await execute(tool, { action: 'poll' })
  assert.equal(sessions.inputs.length, 1)
  assert.equal(sessions.inputs[0].mode, 'steer')
  assert.match(sessions.inputs[0].content[0].text, /review changes_requested by zerob13/)
  assert.match(sessions.inputs[0].content[0].text, /comment by alice/)
})

test('an already accepted submission is not delivered again', async (t) => {
  const { loaded, sessions, state, tool } = await open()
  t.after(() => loaded.unload())

  await execute(tool, { action: 'watch', repo: 'arcships/dim', number: 7 })
  state.comment = { id: 3, user: { login: 'bob' }, body: 'look' }
  state.comments = 1

  await execute(tool, { action: 'poll' })
  assert.equal(sessions.inputs.length, 1)
  // Same signature seen again after a reload: lookup says accepted.
  const before = sessions.inputs.length
  await execute(tool, { action: 'poll' })
  assert.equal(sessions.inputs.length, before)
})

test('unwatch and list reflect the subscriptions; invalid targets are refused', async (t) => {
  const { loaded, tool } = await open()
  t.after(() => loaded.unload())

  await execute(tool, { action: 'watch', repo: 'arcships/dim', number: 361 })
  const listed = await execute(tool, { action: 'list' })
  assert.match(listed.content, /arcships\/dim#361/)

  const removed = await execute(tool, { action: 'unwatch', repo: 'arcships/dim', number: 361 })
  assert.match(removed.content, /Unwatched arcships\/dim#361/)
  assert.equal((await execute(tool, { action: 'list' })).content, 'No GitHub subscriptions in this Session.')

  const invalid = await execute(tool, { action: 'watch', repo: 'not a repo', number: 361 })
  assert.equal(invalid.isError, true)
})

test('configured targets subscribe when the Session binds, without the tool', async (t) => {
  const sessions = fakeSessions()
  const state = { ghCalls: 0 }
  const loaded = await load(githubWatch({ runGh: fakeGh(state) }), {
    config: { intervalSeconds: 3600, targets: [{ repo: 'arcships/dim', number: 361 }] },
    services: { sessions: sessions.service },
  })
  t.after(() => loaded.unload())

  // The host binds the observer with the Session id; the plugin subscribes then.
  loaded.service('sessionEvents:github-watch').bind({ sessionId: 'bound-session', config: {} }, new Promise(() => {}))
  await new Promise((resolve) => setTimeout(resolve, 20))
  assert.equal(state.ghCalls > 0, true)

  state.comment = { id: 9, user: { login: 'carol' }, body: 'fyi' }
  state.comments = 1
  const polled = await loaded.service('toolContribution:githubWatch').execute(toolCall({ action: 'poll' }), new Promise(() => {}))
  assert.match(polled.content, /Delivered changes for: arcships\/dim#361/)
  assert.equal(sessions.inputs.length, 1)
  assert.equal(sessions.inputs[0].sessionId, 'bound-session')
})

test('a control file is how the model adds and removes subscriptions', async (t) => {
  const dir = mkdtempSync(join(tmpdir(), 'github-watch-'))
  const control = join(dir, 'control.json')
  const sessions = fakeSessions()
  const state = { ghCalls: 0 }
  const loaded = await load(githubWatch({ runGh: fakeGh(state) }), {
    config: { intervalSeconds: 3600, controlPath: control },
    services: { sessions: sessions.service },
  })
  t.after(() => loaded.unload())
  loaded.service('sessionEvents:github-watch').bind({ sessionId: 'bound', config: {} }, new Promise(() => {}))

  writeFileSync(control, JSON.stringify({ watch: [{ repo: 'arcships/dim', number: 361 }] }))
  const tool = loaded.service('toolContribution:githubWatch')
  await tool.execute(toolCall({ action: 'poll' }), new Promise(() => {}))
  const status = JSON.parse(readFileSync(join(dir, 'status.json'), 'utf8'))
  assert.equal(status.subscriptions[0].target, 'arcships/dim#361')

  state.comment = { id: 4, user: { login: 'dan' }, body: 'one more' }
  state.comments = 1
  await tool.execute(toolCall({ action: 'poll' }), new Promise(() => {}))
  assert.equal(sessions.inputs.length, 1)
  assert.equal(sessions.inputs[0].sessionId, 'bound')

  writeFileSync(control, JSON.stringify({ unwatch: [{ repo: 'arcships/dim', number: 361 }] }))
  await tool.execute(toolCall({ action: 'poll' }), new Promise(() => {}))
  const after = JSON.parse(readFileSync(join(dir, 'status.json'), 'utf8'))
  assert.deepEqual(after.subscriptions, [])
})

test('unloading stops the poller', async (t) => {
  const { loaded, sessions, state, tool } = await open({ intervalSeconds: 1 })
  await execute(tool, { action: 'watch', repo: 'arcships/dim', number: 361 })
  const calls = state.ghCalls
  await loaded.unload()
  await new Promise((resolve) => setTimeout(resolve, 1200))
  assert.equal(state.ghCalls, calls)
  assert.equal(sessions.inputs.length, 0)
})
