import assert from 'node:assert/strict'
import fs from 'node:fs'
import test from 'node:test'
import { z } from 'zod'
import { load } from '@arcships/rutis/testing'
import processPlugin from './index.ts'
import sessionPlugin from './session.ts'

const ok = value => ({ ok: true, value })
const failure = code => ({ ok: false, error: { code, message: code, retryable: code === 'reloading' } })
const policy = preset => ({ preset, allowedTools: [], disallowedTools: [] })
const flush = () => new Promise(resolve => setImmediate(resolve))
const copy = value => structuredClone(value)
const seed = { name: 'daily', schedule: { at: '2030-01-01T00:00:00Z' }, prompt: 'Check the project', cwd: '/project' }
const vectors = JSON.parse(fs.readFileSync(new URL('../../../tests/fixtures/permission-policy-cap.json', import.meta.url)))

async function until(predicate, message) {
  for (let index = 0; index < 500; index++) {
    if (await predicate()) return
    await flush()
  }
  assert.fail(message)
}

/** Public process/Session services across the strict SDK boundary, with host-owned I/O. */
async function host(t, initial = {}) {
  t.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: initial.now ?? Date.parse('2026-10-22T00:00:00Z') })
  const h = {
    document: { jobs: [], max_concurrent_runs: 4 }, revision: 0, text: null, closed: false,
    defaults: policy('full-access'), policies: new Map([['a', policy('workspace-write')], ['b', policy('full-access')]]),
    launches: [], inputs: [], deletions: [], withdrawals: [], stops: [], updates: [], timelines: new Map(), receipts: new Map(),
    settingsReads: 0, storeReads: 0,
    mode: 'completed', saveConflict: false, permissionFailure: false, reportFailure: undefined,
    ...initial,
  }
  let lock = Promise.resolve(), nextSession = 0
  let pending = Promise.withResolvers()
  h.append = (sessionId, type, name, data) => {
    const events = h.timelines.get(sessionId) ?? []
    const event = { id: `${sessionId}:${events.length + 1}`, sequence: events.length + 1, create_at: Date.now(), type, name, data }
    events.push(event); h.timelines.set(sessionId, events)
    return event
  }
  const scope = {
    async withLock(path, callback) {
      assert.equal(path, 'scheduler/runs.json')
      const previous = lock, release = Promise.withResolvers()
      lock = release.promise
      await previous
      try {
        assert.equal(h.closed, false, 'file scope remains owned through admitted work')
        return ok(await callback({ read: async () => { h.storeReads++; return h.storeReadFailure ? { ok: false, error: 'io' } : ok(h.text) }, replace: async text => {
          if (h.replaceFailure) return { ok: false, error: 'io' }
          h.text = text; return ok(null)
        } }))
      } finally { release.resolve() }
    },
    quiesce() { return null },
    async close() { await lock; h.closed = true; return null },
  }
  const services = {
    managementMethods: { async register(namespace, declarations, call) { h.methods = call } },
    partRegistry: { async registerDefinition(scope, name, definition) { h.part = { scope, name, definition } } },
    settings: {
      async register(declaration) {
        assert.equal(declaration.source, 'schedule')
        h.policySchema = z.fromJSONSchema(declaration.schema.properties.jobs.items.properties.policy_cap)
      },
      async read() {
        h.settingsReads++
        const snapshot = { revision: String(h.revision), document: copy(h.document) }
        if (h.readGate) {
          const gate = h.readGate
          h.readGate = undefined
          await gate.promise
        }
        return h.readFailure ? failure('failed') : ok(snapshot)
      },
      async update(request) {
        h.updates.push(copy(request))
        if (h.updateGate) {
          const gate = h.updateGate
          h.updateGate = undefined
          await gate.promise
        }
        if (h.saveConflict || request.expectedRevision !== String(h.revision)) return failure('conflict')
        if (h.writeFailure) return failure(h.writeFailure)
        assert.equal(request.document.max_concurrent_runs, h.document.max_concurrent_runs)
        for (const job of request.document.jobs) h.policySchema.parse(job.policy_cap)
        h.document = copy(request.document); h.revision++
        await h.owner.parallel('settings/schedule', { source: 'schedule' })
        if (h.afterWriteFailure === 'bridge') throw new Error('reply lost after commit')
        if (h.afterWriteFailure) return failure(h.afterWriteFailure)
        return ok({ revision: String(h.revision), document: copy(h.document) })
      },
    },
    sessionPermissions: {
      async permission({ sessionId }) {
        if (h.permissionGate && sessionId === h.permissionGateFor) await h.permissionGate.promise
        if (h.permissionFailure || !h.policies.has(sessionId)) throw new Error('permission unavailable')
        return { version: 1, policy: copy(h.policies.get(sessionId)) }
      },
      async permissionDefaults() { if (h.permissionFailure) throw new Error('permission unavailable'); return copy(h.defaults) },
    },
    pluginFiles: { async open() { h.closed = false; return scope } },
    sessions: {
      async launch(request) {
        h.launches.push(copy(request))
        if (h.launchGate) await h.launchGate.promise
        const sessionId = `s${++nextSession}`, actual = h.launchPolicy ?? request.policy
        h.policies.set(sessionId, copy(actual))
        return ok({ sessionId, policy: copy(actual) })
      },
      async input(request) {
        h.inputs.push(copy(request))
        if (request.submissionId.startsWith('scheduler-report:')) {
          if (h.reportGate) await h.reportGate.promise
          if (h.reportFailure) return failure(h.reportFailure)
        }
        const event = h.append(request.sessionId, 'input', 'message', {})
        const receipt = { inputId: event.id, sequence: event.sequence }
        h.receipts.set(request.submissionId, receipt)
        if (request.mode === 'queue') {
          if (h.mode !== 'held') h.append(request.sessionId, 'input', 'applied', { input_id: event.id })
          if (h.mode === 'interrupted-before-consumption') h.append(request.sessionId, 'stop', 'interrupt', {})
          if (['completed', 'failed', 'cancelled', 'running'].includes(h.mode)) h.append(request.sessionId, 'input', 'consumed', { input_id: event.id })
          if (['completed', 'failed', 'cancelled'].includes(h.mode)) h.append(request.sessionId, 'turn', 'end', { status: h.mode })
          if (h.inputGate) await h.inputGate.promise
          if (h.inputReplyLost) { h.inputReplyLost = false; throw new Error('input reply lost after acceptance') }
        }
        return ok(receipt)
      },
      async lookup(request) {
        if (h.lookupFailures > 0) { h.lookupFailures--; throw new Error('lookup bridge unavailable') }
        const receipt = h.receipts.get(request.submissionId)
        return ok(receipt ? { status: 'accepted', ...receipt } : { status: 'unknown' })
      },
      async history(request) {
        if (h.historyFailure) throw new Error('history bridge unavailable')
        const events = h.timelines.get(request.sessionId) ?? []
        const through = request.through ?? events.length
        return ok({ through, events: copy(events.filter(event => event.sequence > request.after && event.sequence <= through).slice(0, request.limit)) })
      },
      async withdraw(request) {
        h.withdrawals.push(copy(request))
        if (h.withdrawMode === 'prior-deletion') {
          h.append(request.sessionId, 'input', 'deleted', { input_id: request.inputId })
          return ok({ status: 'notHeld' })
        }
        const consumed = (h.timelines.get(request.sessionId) ?? []).some(event => event.type === 'input' && event.name === 'consumed' && event.data.input_id === request.inputId)
        if (consumed || h.mode !== 'held') return ok({ status: 'notHeld' })
        const event = h.append(request.sessionId, 'input', 'deleted', { input_id: request.inputId })
        return ok({ status: 'deleted', sequence: event.sequence })
      },
      async stop(request) {
        h.stops.push(copy(request))
        if (h.stopReloads > 0) { h.stopReloads--; return failure('reloading') }
        h.append(request.sessionId, 'stop', 'interrupt', {})
        const event = h.append(request.sessionId, 'turn', 'end', { status: 'cancelled' })
        return ok({ sequence: event.sequence })
      },
      async delete(request) {
        h.deletions.push(copy(request))
        if (h.deleteGate) await h.deleteGate.promise
        return h.deleteFailure ? failure('failed') : ok({})
      },
    },
  }
  h.owner = await load(processPlugin, { services, config: {} })
  h.services = services
  const toolRegistry = { async register(declaration, execute) { h.contribution = { declaration, execute } } }
  assert.equal(h.part.scope, 'session')
  const guideEntries = { entries: [], async register(scope, entry) { this.entries.push({ scope, entry: copy(entry) }) } }
  h.session = await load(h.part.definition, { services: { scheduleJobs: h.owner.service('scheduleJobs'), toolRegistry, guideEntries } })
  h.call = async (name, params = {}) => {
    const reply = await h.methods({ callId: 'm', method: `schedule.${name}`, params }, pending.promise, () => {})
    assert.equal(reply.kind, 'value', JSON.stringify(reply))
    return reply.value
  }
  h.tool = async (arguments_, sessionId = 'a', config = {}) => h.contribution.execute({
    callId: 'tool', turnId: 'turn', sessionId, branchId: 'main', arguments: arguments_, config: {
      permissionPolicy: h.policies.get(sessionId), cwd: '/project', model: { providerId: 'test', modelId: 'test' },
      systemPrompt: '', tools: ['schedule'], collaborationRole: 'leader', ...config,
    },
  }, pending.promise)
  h.runs = () => h.text === null ? [] : JSON.parse(h.text).runs
  h.advance = async milliseconds => { t.mock.timers.tick(milliseconds); await flush() }
  h.close = async () => {
    if (h.closed) return
    pending.resolve()
    await h.session.unload()
    let settled = false
    const closing = h.owner.unload().then(() => { settled = true })
    for (let index = 0; !settled && index < 10; index++) { await flush(); t.mock.timers.tick(1000) }
    await closing
  }
  h.reopen = async () => {
    await h.close()
    pending = Promise.withResolvers()
    h.owner = await load(processPlugin, { services, config: {} })
    const guideEntries = { entries: [], async register(scope, entry) { this.entries.push({ scope, entry: copy(entry) }) } }
  h.session = await load(h.part.definition, { services: { scheduleJobs: h.owner.service('scheduleJobs'), toolRegistry, guideEntries } })
    await flush()
  }
  t.after(async () => {
    for (const gate of [h.launchGate, h.inputGate, h.reportGate, h.permissionGate, h.deleteGate]) gate?.resolve()
    await h.close()
  })
  await flush()
  return h
}

test('authorizing saves replace origin/cap together, disclose the transition, and preserve accepted reporting', async t => {
  const h = await host(t, { inputGate: Promise.withResolvers() })
  const first = await h.tool({ action: 'create', job: seed })
  assert.equal(first.isError, undefined)
  const a = JSON.parse(first.content).job
  assert.deepEqual(a.created_by, { tool: { session_id: 'a', branch_id: 'main' } })
  const accepted = await h.call('run_now', { job_id: a.id })
  await until(() => h.inputs.some(input => input.mode === 'queue'), 'original run input admitted')
  const management = await h.call('save', { job: { id: a.id, name: 'operator' } })
  assert.equal(management.job.created_by, 'user')
  assert.equal(management.transition.to.effective_policy.preset, 'read-only')
  const takeover = await h.tool({ action: 'update', job_id: a.id, job: { name: 'branch B' } }, 'b')
  const transition = JSON.parse(takeover.content).transition
  assert.equal(transition.from.effective_policy.preset, 'read-only')
  assert.equal(transition.to.effective_policy.preset, 'full-access')
  assert.deepEqual(transition.to.report_to, { session_id: 'b', branch_id: 'main' })
  assert.match(h.contribution.declaration.description, /even a rename/)
  h.inputGate.resolve()
  await until(() => h.inputs.some(input => input.mode === 'steer'), 'original report published')
  const report = h.inputs.find(input => input.mode === 'steer')
  assert.equal(report.sessionId, 'a')
  assert.match(report.rawBlocks[0].text, new RegExp(accepted.run_id))
  assert.deepEqual(report.content, [])
  assert.equal(h.runs().find(run => run.run_id === accepted.run_id).state, 'completed')
  const next = await h.call('run_now', { job_id: a.id })
  await until(() => h.inputs.some(input => input.submissionId === `scheduler-report:${next.run_id}`), 'future run reports to B')
  assert.equal(h.launches.at(-1).policy.preset, 'full-access')
  assert.equal(h.inputs.at(-1).sessionId, 'b')
})

test('failed policy reads, above-cap saves, and document CAS conflicts preserve the job', async t => {
  const h = await host(t)
  const original = (await h.call('save', { job: seed })).job
  const before = copy(h.document)
  h.policies.set('a', policy('read-only'))
  const rejected = await h.tool({ action: 'update', job_id: original.id, job: { policy: 'full-access' } })
  assert.equal(rejected.isError, true)
  assert.match(rejected.content, /schedule_invalid/)
  assert.deepEqual(h.document, before)
  h.permissionFailure = true
  await assert.rejects(h.tool({ action: 'update', job_id: original.id, job: { name: 'not saved' } }), /permission unavailable/)
  assert.deepEqual(h.document, before)
  h.permissionFailure = false; h.saveConflict = true
  const conflict = await h.tool({ action: 'update', job_id: original.id, job: { name: 'stale' } })
  assert.equal(conflict.isError, true); assert.match(conflict.content, /schedule_conflict/)
  assert.deepEqual(h.document, before)
})

test('a deadline outside inner-v1 timestamps rejects saving without changing jobs or stored runs', async t => {
  const h = await host(t)
  const ordinary = (await h.call('save', { job: seed })).job
  const accepted = await h.call('run_now', { job_id: ordinary.id })
  await until(() => h.runs().some(run => run.run_id === accepted.run_id && run.state === 'completed'), 'first run persisted')
  const before = h.text, jobs = copy(h.document), launches = h.launches.length
  const rejected = await h.tool({ action: 'create', job: { ...seed, max_run_minutes: 5_000_000_000 } })
  assert.equal(rejected.isError, true)
  assert.match(rejected.content, /schedule_invalid/)
  assert.equal(h.text, before)
  assert.deepEqual(h.document, jobs)
  assert.equal(h.launches.length, launches)
  assert.equal((await h.call('list')).jobs.length, 1)
})

test('retiring a Session handler rejects retained and in-flight calls without claiming rollback', async () => {
  for (const rejects of [false, true]) {
    const gate = Promise.withResolvers(), entered = Promise.withResolvers()
    let calls = 0
    let tool
    const session = await load(sessionPlugin(), { services: { guideEntries: { async register() {} }, toolRegistry: { async register(declaration, execute) { tool = { declaration, execute } } }, scheduleJobs: { async call() {
      calls++; entered.resolve(); await gate.promise
      if (rejects) throw new Error('owner retired')
      return { content: 'saved', records: { saved: true } }
    } } } })
    const call = { callId: 'tool', turnId: 'turn', sessionId: 'a', branchId: 'main',
      arguments: { action: 'list' }, config: { cwd: '/project', collaborationRole: 'leader' } }
    const cancellation = Promise.withResolvers()
    const pending = tool.execute(call, cancellation.promise)
    await entered.promise
    await session.unload()
    gate.resolve()
    const late = await pending
    assert.equal(late.isError, true)
    assert.match(late.content, /may already have taken effect/)
    const retained = await tool.execute(call, cancellation.promise)
    assert.equal(retained.isError, true)
    assert.match(retained.content, /this call was not made/)
    assert.equal(calls, 1)
    cancellation.resolve()
  }
})

test('job preset saves and launched policies follow the shared cap contract', async t => {
  const h = await host(t)
  for (const vector of vectors.cap.filter(vector => vector.usage === 'job-save-and-launch')) {
    h.policies.set('a', vector.bound)
    const reply = await h.tool({ action: 'create', job: { ...seed, name: vector.name, policy: vector.requested.preset } })
    if (!vector.presetWithin) { assert.equal(reply.isError, true, vector.name); continue }
    const saved = JSON.parse(reply.content)
    assert.deepEqual(saved.transition.to.effective_policy, vector.capped, vector.name)
    const run = await h.call('run_now', { job_id: saved.job.id })
    await until(() => h.runs().find(value => value.run_id === run.run_id)?.state === 'completed', vector.name)
    assert.deepEqual(h.launches.at(-1).policy, vector.capped, vector.name)
  }
  for (const vector of vectors.reject.filter(vector => vector.usage === 'job-save-and-launch')) {
    h.policies.set('a', vector.bound)
    const before = copy(h.document), writes = h.updates.length
    for (const request of [
      { action: 'create', job: { ...seed, policy: vector.requested.preset } },
      { action: 'update', job_id: h.document.jobs.at(-1).id, job: { policy: vector.requested.preset } },
    ]) {
      const reply = await h.tool(request)
      assert.equal(reply.isError, true, vector.name)
      assert.match(reply.content, /schedule_invalid/)
    }
    assert.deepEqual(h.document, before)
    assert.equal(h.updates.length, writes, 'unrepresentable caps fail before the Settings CAS')
  }
  const invalid = h.document.jobs.at(-1)
  invalid.policy = 'read-only'
  h.policies.set('a', policy('full-access'))
  const before = copy(h.document), writes = h.updates.length
  const reply = await h.tool({ action: 'update', job_id: invalid.id, job: { policy: 'workspace-write' } })
  assert.equal(reply.isError, true)
  assert.deepEqual(h.document, before, 'an unrepresentable old transition also fails before commit')
  assert.equal(h.updates.length, writes)
  const launches = h.launches.length, inputs = h.inputs.filter(input => input.mode === 'queue').length
  const run = await h.call('run_now', { job_id: invalid.id })
  await until(() => h.runs().find(value => value.run_id === run.run_id)?.state === 'failed', 'configured invalid cap fails closed')
  assert.equal(h.runs().find(value => value.run_id === run.run_id).detail, 'schedule_invalid')
  assert.equal(h.launches.length, launches)
  assert.equal(h.inputs.filter(input => input.mode === 'queue').length, inputs)
})

test('tool scope and schema are enforced before the process owner sees a save', async t => {
  const h = await host(t)
  const declaration = h.contribution.declaration
  assert.equal(declaration.risk, 'persistent_configuration')
  assert.equal(declaration.inputSchema.properties.job.properties.schedule.type, 'object',
    'schedule is an anyOf union; the native tool schema requires its object root type')
  for (const config of [{ interactive: false }, { collaborationRole: 'member' }]) {
    const reply = await h.tool({ action: 'create', job: seed }, 'a', config)
    assert.equal(reply.isError, true)
  }
  const forged = await h.tool({ action: 'create', job: { ...seed, policy_cap: policy('full-access') } })
  assert.equal(forged.isError, true)
  assert.deepEqual(h.document.jobs, [])
})

test('reuse checks every shared full-policy vector and both independent saved/live bounds before input', async t => {
  const h = await host(t)
  const cases = [
    ...[...vectors.cap, ...vectors.reject].flatMap(vector => [
      { name: `${vector.name} saved`, x: vector.requested, savedEffective: vector.bound,
        liveBound: policy('full-access'), permitted: vector.within ?? false },
      { name: `${vector.name} live`, x: vector.requested, savedEffective: vector.requested,
        liveBound: vector.bound, permitted: vector.within ?? false },
    ]),
    ...vectors.reuse.map(vector => ({ ...vector, permitted: vector.withinSaved && vector.withinLive })),
  ]
  for (const [index, vector] of cases.entries()) {
    const sessionId = `reuse${index}`, jobId = `reuse-job${index}`
    h.policies.set(sessionId, copy(vector.x)); h.policies.set('a', copy(vector.liveBound))
    h.document.jobs.push({ ...seed, id: jobId, mode: 'reuse', session_id: sessionId,
      enabled: true, enabled_at: new Date().toISOString(), revision: 1, max_run_minutes: 60,
      created_by: { tool: { session_id: 'a', branch_id: 'main' } }, policy_cap: copy(vector.savedEffective),
    })
    const run = await h.call('run_now', { job_id: jobId })
    await until(() => ['completed', 'skipped'].includes(h.runs().find(value => value.run_id === run.run_id)?.state), vector.name)
    const actual = h.runs().find(value => value.run_id === run.run_id)
    assert.equal(actual.state, vector.permitted ? 'completed' : 'skipped', vector.name)
    assert.equal(h.inputs.some(input => input.submissionId === run.submission_id), vector.permitted, vector.name)
    assert.deepEqual(h.policies.get(sessionId), vector.x, 'reuse must never mutate policy')
    if (vector.permitted) assert.equal(actual.session_origin, 'reuse')
  }
  assert.equal(h.launches.length, 0)
})

test('cron skips a nonexistent DST wall time instead of running at the library-shifted time', async t => {
  const h = await host(t, { now: Date.parse('2026-03-08T06:59:00Z') })
  const job = (await h.call('save', { job: { ...seed, schedule: { cron: '30 2 * * *', tz: 'America/New_York' } } })).job
  await h.advance(31 * 60_000)
  assert.deepEqual((await h.call('history', { job_id: job.id })).runs, [])
  assert.equal(h.launches.length, 0)
  await h.advance(23 * 60 * 60_000)
  await until(() => h.runs().some(run => run.state === 'completed'), 'next real 02:30 runs')
  assert.equal(h.runs()[0].window, '2026-03-09T06:30:00.000Z')
})

test('cron runs a repeated DST wall time once and resumes on the next day', async t => {
  const h = await host(t, { now: Date.parse('2026-11-01T05:29:00Z') })
  await h.call('save', { job: { ...seed, schedule: { cron: '30 1 * * *', tz: 'America/New_York' } } })
  await h.advance(60_000)
  await until(() => h.runs()[0]?.state === 'completed', 'first 01:30 completes')
  await h.advance(60 * 60_000)
  assert.equal(h.runs().length, 1, 'second occurrence cannot fire again')
  await h.advance(24 * 60 * 60_000)
  await until(() => h.runs().length === 2 && h.runs()[1].state === 'completed', 'following day runs')
  assert.deepEqual(h.runs().map(run => run.window), ['2026-11-01T05:30:00.000Z', '2026-11-02T06:30:00.000Z'])
})

test('every windows preserve the enabled anchor through edits and reset it only on re-enable', async t => {
  const h = await host(t)
  const job = (await h.call('save', { job: { ...seed, schedule: { every_minutes: 1 } } })).job
  await h.advance(1000)
  await until(() => h.runs()[0]?.state === 'completed', 'anchor window completes')
  const renamed = await h.call('save', { job: { id: job.id, name: 'renamed' } })
  assert.equal(renamed.job.enabled_at, job.enabled_at)
  await h.advance(59_000)
  await until(() => h.runs().length === 2 && h.runs()[1].state === 'completed', 'second anchor window completes')
  await h.call('set_enabled', { job_id: job.id, enabled: false })
  assert.equal('enabled_at' in h.document.jobs[0], false)
  await h.advance(120_000)
  assert.equal(h.runs().length, 2)
  const enabled = (await h.call('set_enabled', { job_id: job.id, enabled: true })).job
  assert.equal(enabled.enabled_at, new Date().toISOString())
  assert.notEqual(enabled.enabled_at, job.enabled_at)
  await h.advance(1000)
  await until(() => h.runs().length === 3, 'new enable anchor fires')
  assert.equal(h.runs()[2].window, enabled.enabled_at)
})

test('clock jumps collapse missed windows without catch-up and one-off jobs disable themselves', async t => {
  const h = await host(t)
  const repeated = (await h.call('save', { job: { ...seed, schedule: { every_minutes: 2 } } })).job
  await until(() => h.runs()[0]?.state === 'completed', 'the enable anchor runs through Settings change delivery')
  const before = h.runs().length, launches = h.launches.length
  await h.advance(11 * 60_000)
  assert.equal(h.runs().length, before + 1)
  assert.equal(h.runs().at(-1).state, 'missed')
  assert.equal(h.launches.length, launches)
  await h.call('set_enabled', { job_id: repeated.id, enabled: false })
  const at = new Date(Date.now() + 60_000).toISOString()
  const once = (await h.call('save', { job: { ...seed, schedule: { at } } })).job
  await h.advance(60_000)
  await until(() => h.runs().some(run => run.job_id === once.id && run.state === 'completed'), 'one-off run completes')
  await h.advance(1000)
  await until(() => h.document.jobs.find(job => job.id === once.id).enabled === false, 'one-off disables')
  assert.equal('enabled_at' in h.document.jobs.find(job => job.id === once.id), false)
  await h.advance(60_000)
  await h.call('set_enabled', { job_id: once.id, enabled: true })
  await h.advance(1000)
  await until(() => h.document.jobs.find(job => job.id === once.id).enabled === false, 'reenabling the expired one-off does not replay it')
  assert.equal(h.runs().filter(run => run.job_id === once.id).length, 1)

  const beforeWake = h.runs().filter(run => run.job_id === repeated.id && run.state === 'completed').length
  h.document.jobs = [
    { ...h.document.jobs.find(job => job.id === once.id), enabled: true, enabled_at: new Date().toISOString() },
    { ...h.document.jobs.find(job => job.id === repeated.id), enabled: true, enabled_at: new Date().toISOString() },
  ]
  h.revision++
  h.saveConflict = true
  await h.owner.parallel('settings/schedule', { source: 'schedule' })
  await until(() => h.runs().filter(run => run.job_id === repeated.id && run.state === 'completed').length > beforeWake,
    'a wake disable conflict leaves the next job running without owner reload')
  assert.equal((await h.call('list')).jobs[0].enabled, true)
  h.saveConflict = false
  await h.owner.parallel('settings/schedule', { source: 'schedule' })
  await until(() => h.document.jobs[0].enabled === false, 'a later wake settles the one-off disable')
})

test('held, consumed and interrupted-before-consumption deadlines settle through the owning interfaces', async t => {
  const h = await host(t)
  for (const mode of ['held', 'running', 'interrupted-before-consumption']) {
    h.mode = mode
    const job = JSON.parse((await h.tool({ action: 'create', job: { ...seed, max_run_minutes: 1 } })).content).job
    const run = await h.call('run_now', { job_id: job.id })
    await until(() => h.runs().find(value => value.run_id === run.run_id)?.input_event_id, mode)
    await flush()
    const withdrawals = h.withdrawals.length, stops = h.stops.length
    await h.advance(60_000)
    if (mode === 'running') await h.advance(1000)
    await until(() => h.runs().find(value => value.run_id === run.run_id)?.state === 'timed_out', mode)
    assert.equal(h.withdrawals.length - withdrawals, mode === 'held' ? 1 : 0, mode)
    assert.equal(h.stops.length - stops, mode === 'running' ? 1 : 0, mode)
  }
})

test('notHeld followed by a concurrent deletion settles withdrawn instead of assuming consumption', async t => {
  const h = await host(t, { mode: 'held', withdrawMode: 'prior-deletion' })
  const job = (await h.call('save', { job: { ...seed, max_run_minutes: 1 } })).job
  const run = await h.call('run_now', { job_id: job.id })
  await until(() => h.runs()[0]?.input_event_id, 'held receipt persisted')
  await h.advance(60_000)
  await h.advance(1000)
  await until(() => h.runs()[0]?.state === 'cancelled', 'prior deletion is preserved')
  assert.equal(h.runs()[0].run_id, run.run_id)
  assert.equal(h.runs()[0].detail, 'withdrawn')
  assert.equal(h.stops.length, 0)
})

test('an admitted launch that returns after the deadline persists ownership without input or stop', async t => {
  const h = await host(t, { launchGate: Promise.withResolvers() })
  const job = (await h.call('save', { job: { ...seed, max_run_minutes: 1 } })).job
  await h.call('run_now', { job_id: job.id })
  await until(() => h.launches.length === 1, 'launch admitted')
  await h.advance(60_000)
  h.launchGate.resolve()
  await until(() => h.runs()[0]?.state === 'timed_out', 'late launch settled')
  assert.equal(h.runs()[0].session_origin, 'launch')
  assert.equal(h.runs()[0].session_id, 's1')
  assert.deepEqual(h.runs()[0].policy, policy('read-only'))
  assert.equal(h.inputs.length, 0)
  assert.equal(h.withdrawals.length, 0)
  assert.equal(h.stops.length, 0)
})

test('failed reports are single-attempt and unload retains the admitted report and record scope', async t => {
  const h = await host(t, { reportGate: Promise.withResolvers(), reportFailure: 'publication_unknown' })
  const job = JSON.parse((await h.tool({ action: 'create', job: seed })).content).job
  const run = await h.call('run_now', { job_id: job.id })
  await until(() => h.inputs.some(input => input.mode === 'steer'), 'report admitted')
  const closing = h.close()
  await flush()
  assert.equal(h.closed, false)
  h.reportGate.resolve()
  await closing
  assert.equal(h.closed, true)
  assert.equal(h.runs()[0].state, 'completed')
  assert.match(h.runs()[0].detail, /report_failed: publication_unknown/)
  assert.equal(h.inputs.filter(input => input.submissionId === `scheduler-report:${run.run_id}`).length, 1)
  await h.reopen()
  assert.equal(h.inputs.filter(input => input.submissionId === `scheduler-report:${run.run_id}`).length, 1, 'reload never replays terminal reports')
})

test('a job revision change skips an admitted launch, while editing another job does not', async t => {
  const h = await host(t, { launchGate: Promise.withResolvers() })
  const job = (await h.call('save', { job: seed })).job
  const other = (await h.call('save', { job: { ...seed, name: 'other' } })).job
  await h.call('run_now', { job_id: job.id })
  await until(() => h.launches.length === 1, 'first launch admitted')
  await h.call('save', { job: { id: job.id, name: 'changed during launch' } })
  h.launchGate.resolve()
  await until(() => h.runs()[0]?.state === 'skipped', 'changed snapshot is skipped')
  assert.equal(h.runs()[0].detail, 'changed')
  assert.equal(h.runs()[0].session_origin, 'launch')
  assert.equal(h.inputs.length, 0)
  h.launchGate = Promise.withResolvers()
  const second = await h.call('run_now', { job_id: job.id })
  await until(() => h.launches.length === 2, 'second launch admitted')
  await h.call('save', { job: { id: other.id, name: 'unrelated change' } })
  h.launchGate.resolve()
  await until(() => h.runs().find(run => run.run_id === second.run_id)?.state === 'completed', 'other job revision is irrelevant')
  assert.equal(h.inputs.filter(input => input.mode === 'queue').length, 1)
})

test('a reused policy reply received after the deadline is recorded without granting ownership or input', async t => {
  const h = await host(t, { permissionGate: Promise.withResolvers(), permissionGateFor: 'reuse' })
  h.policies.set('reuse', policy('read-only'))
  const job = (await h.call('save', { job: { ...seed, mode: 'reuse', session_id: 'reuse', max_run_minutes: 1 } })).job
  await h.call('run_now', { job_id: job.id })
  await until(() => h.runs()[0]?.state === 'launching', 'reuse accepted')
  await flush()
  await h.advance(60_000)
  h.permissionGate.resolve()
  await until(() => h.runs()[0]?.state === 'timed_out', 'late reused policy settles')
  assert.equal(h.runs()[0].session_origin, 'reuse')
  assert.equal(h.inputs.length + h.withdrawals.length + h.stops.length, 0)
  assert.equal(h.launches.length, 0)
})

test('reload resumes accepted history without duplicate input, deadline reset or terminal report replay', async t => {
  const h = await host(t, { mode: 'running' })
  const explicit = { ...policy('full-access'), fileAccess: 'read-only', toolApproval: 'bypass' }
  h.policies.set('a', explicit)
  const job = JSON.parse((await h.tool({ action: 'create', job: seed })).content).job
  assert.deepEqual(job.policy_cap, explicit)
  const run = await h.call('run_now', { job_id: job.id })
  await until(() => h.runs()[0]?.state === 'running', 'run is tracked before reload')
  const before = copy(h.runs()[0])
  assert.deepEqual(before.policy, explicit)
  await h.reopen()
  h.append(before.session_id, 'turn', 'end', { status: 'completed' })
  await h.advance(1000)
  await until(() => h.runs()[0]?.state === 'completed', 'original run completes after reload')
  await until(() => h.inputs.some(input => input.mode === 'steer'), 'one original report')
  assert.equal(h.inputs.filter(input => input.mode === 'queue').length, 1)
  assert.equal(h.runs()[0].deadline_at, before.deadline_at)
  assert.equal(h.runs()[0].run_id, run.run_id)
  assert.deepEqual(h.runs()[0].policy, explicit)
  assert.equal(JSON.parse(h.text).version, 1)
  await h.reopen()
  assert.equal(h.inputs.filter(input => input.mode === 'steer').length, 1)
  assert.equal(h.document.jobs[0].enabled_at, job.enabled_at)
})

test('selected terminal results report only for tool origins, while disabled windows stay history-only', async t => {
  const h = await host(t)
  for (const origin of ['user', 'tool']) {
    const job = origin === 'user' ? (await h.call('save', { job: seed })).job
      : JSON.parse((await h.tool({ action: 'create', job: seed })).content).job
    for (const state of ['completed', 'failed', 'cancelled']) {
      h.mode = state
      const run = await h.call('run_now', { job_id: job.id })
      await until(() => h.runs().find(value => value.run_id === run.run_id)?.state === state, `${origin}/${state}`)
      await flush()
      assert.equal(h.inputs.filter(input => input.submissionId === `scheduler-report:${run.run_id}`).length, origin === 'tool' ? 1 : 0)
    }
    await h.call('set_enabled', { job_id: job.id, enabled: false })
    const skipped = await h.call('run_now', { job_id: job.id })
    assert.equal(skipped.state, 'skipped')
    assert.equal('report_to' in skipped, false)
    assert.equal(h.inputs.filter(input => input.submissionId === `scheduler-report:${skipped.run_id}`).length, 0)
  }
})

test('retention keeps referenced ownership and retries deletion and failed record removal without another job', async t => {
  const anchor = '2026-10-22T00:00:00.000Z'
  const job = { ...seed, id: 'retained', mode: 'create', enabled: true, enabled_at: anchor, revision: 1,
    max_run_minutes: 60, created_by: 'user', policy_cap: policy('full-access') }
  const runs = Array.from({ length: 102 }, (_, index) => {
    const runId = `old-${index}`
    const at = new Date(Date.parse(anchor) - (102 - index) * 1000).toISOString()
    return { run_id: runId, job_id: job.id, window: 'manual', state: 'completed', session_id: `owned-${index}`, session_origin: 'launch',
      policy: policy('read-only'), input_event_id: `input-${index}`, submission_id: `scheduler:${runId}`, created_at: at, ended_at: at,
      deadline_at: new Date(Date.parse(at) + 60_000).toISOString(), detail: 'completed' }
  })
  const h = await host(t, { document: { jobs: [job, { ...job, id: 'reuse', mode: 'reuse', session_id: 'owned-0' }], max_concurrent_runs: 4 },
    text: JSON.stringify({ version: 1, runs }), deleteFailure: true })
  assert.equal(h.runs().length, 102)
  assert.deepEqual(h.deletions, [{ sessionId: 'owned-1' }])
  h.document.jobs = []
  h.readFailure = true
  await h.advance(60_000)
  assert.equal(h.deletions.length, 1, 'unreadable Settings never releases the reuse reference')
  h.readFailure = false; h.storeReadFailure = true
  const before = h.text
  await h.advance(60_000)
  assert.equal(h.deletions.length, 1, 'an unreadable run file stops effects without disabling cleanup retries')
  assert.equal(h.text, before)
  assert.equal(h.launches.length, 0)
  h.storeReadFailure = false; h.deleteFailure = false; h.replaceFailure = true
  await h.advance(60_000)
  await until(() => h.deletions.length >= 103, 'owned Sessions deleted before removing their records')
  assert.equal(h.runs().length, 102, 'failed replacement preserves ownership evidence')
  h.replaceFailure = false
  await h.advance(60_000)
  await until(() => h.runs().length === 0, 'empty-job cleanup retries without a separate queue')
  assert.equal(h.deletions.some(request => request.sessionId === 'foreign'), false)
  await flush()
  const reads = { settings: h.settingsReads, runs: h.storeReads }
  await h.advance(60_000)
  assert.equal(h.settingsReads - reads.settings, 1, 'successful cleanup leaves only window evaluation')
  assert.equal(h.storeReads - reads.runs, 1, 'retention no longer scans after its retry succeeds')

  h.mode = 'running'
  const pending = []
  for (const name of ['first', 'second']) {
    const saved = await h.call('save', { job: { ...seed, name } })
    pending.push(await h.call('run_now', { job_id: saved.job.id }))
  }
  await until(() => h.runs().every(run => run.input_event_id), 'both runs accepted before deleting their jobs')
  for (const run of pending) await h.call('delete', { job_id: run.job_id })
  const deletions = h.deletions.length
  h.deleteGate = Promise.withResolvers()
  h.append(h.runs()[0].session_id, 'turn', 'end', { status: 'completed' })
  await h.advance(1000)
  await until(() => h.deletions.length > deletions, 'first completed run cleanup admitted')
  h.append(h.runs()[1].session_id, 'turn', 'end', { status: 'completed' })
  await h.advance(1000)
  await until(() => h.runs().every(run => run.state === 'completed'), 'second run completes during native deletion')
  h.deleteGate.resolve()
  await until(() => h.runs().length === 1, 'first cleanup retains its in-flight snapshot reference')
  await h.advance(1000)
  await until(() => h.runs().length === 0, 'completion during cleanup schedules the necessary follow-up pass')
})

test('a transient Settings recheck failure and history bridge error leave other runs tracking', async t => {
  const h = await host(t, { mode: 'running' })
  const b = (await h.call('save', { job: { ...seed, name: 'already running' } })).job
  const running = await h.call('run_now', { job_id: b.id })
  await until(() => h.runs().find(run => run.run_id === running.run_id)?.input_event_id, 'B input recorded')
  const bRun = copy(h.runs().find(run => run.run_id === running.run_id))
  h.launchGate = Promise.withResolvers()
  const a = (await h.call('save', { job: { ...seed, name: 'launching' } })).job
  const launching = await h.call('run_now', { job_id: a.id })
  await until(() => h.launches.length === 2, 'A launch admitted')
  h.readFailure = true
  h.launchGate.resolve()
  await until(() => h.runs().find(run => run.run_id === launching.run_id)?.state === 'skipped', 'only A is skipped')
  assert.equal(h.runs().find(run => run.run_id === launching.run_id).detail, 'settings_read_failed')
  assert.equal(h.inputs.filter(input => input.mode === 'queue').length, 1)
  h.historyFailure = true
  await h.advance(1000)
  assert.ok(['submitted', 'running'].includes(h.runs().find(run => run.run_id === running.run_id).state))
  h.historyFailure = false
  h.append(bRun.session_id, 'turn', 'end', { status: 'completed' })
  await h.advance(1000)
  await until(() => h.runs().find(run => run.run_id === running.run_id)?.state === 'completed', 'B completes while Settings is unavailable')
  h.readFailure = false
  assert.equal((await h.call('list')).jobs.length, 2)
  h.mode = 'completed'
  const recovered = await h.call('run_now', { job_id: a.id })
  await until(() => h.runs().find(run => run.run_id === recovered.run_id)?.state === 'completed', 'new work runs without owner reload')
})

test('tool results omit list prompts and preserve identities and authority alongside bounded previews', async t => {
  const h = await host(t)
  const prompt = 'x'.repeat(32 * 1024), ids = []
  for (let index = 0; index < 20; index++) ids.push((await h.call('save', { job: { ...seed, name: `job-${index}`, prompt } })).job.id)
  const listed = await h.tool({ action: 'list' })
  const visible = JSON.parse(listed.content)
  assert.deepEqual(visible.jobs.map(job => job.id), ids)
  assert.ok(visible.jobs.every(job => !('prompt' in job)))
  assert.ok(Buffer.byteLength(listed.content) < 32 * 1024)
  assert.equal(listed.structuredContent.jobs[0].prompt, prompt)
  const got = await h.tool({ action: 'get', job_id: ids[0] })
  assert.equal(JSON.parse(got.content).job.prompt_truncated, true)
  assert.ok(Buffer.byteLength(JSON.parse(got.content).job.prompt) <= 4 * 1024)
  assert.equal(got.structuredContent.job.prompt, prompt)
  const saved = await h.tool({ action: 'update', job_id: ids[0], job: { prompt: '汉'.repeat(10_000) } })
  const result = JSON.parse(saved.content)
  assert.equal(result.job.id, ids[0])
  assert.ok(Buffer.byteLength(result.job.prompt) <= 4 * 1024)
  assert.match(result.job.prompt, /^汉+$/)
  assert.deepEqual(result.transition.to, { origin: { tool: { session_id: 'a', branch_id: 'main' } },
    cap: policy('workspace-write'), effective_policy: policy('workspace-write'), report_to: { session_id: 'a', branch_id: 'main' } })
  assert.equal(saved.structuredContent.job.prompt, '汉'.repeat(10_000))
})

test('timer drift does not turn millisecond-offset windows into missed runs', async t => {
  const h = await host(t)
  await h.advance(100)
  const job = (await h.call('save', { job: { ...seed, schedule: { every_minutes: 1 } } })).job
  await until(() => h.runs().some(run => run.state === 'completed'), 'Settings change runs the enabled window')
  await h.advance(900)
  for (let second = 0; second < 119; second++) await h.advance(1001)
  await until(() => h.runs().filter(run => run.job_id === job.id && run.state === 'completed').length === 3,
    'all three windows execute despite a drifting poll offset')
  assert.deepEqual(h.runs().map(run => run.window), [
    '2026-10-22T00:00:00.100Z', '2026-10-22T00:01:00.100Z', '2026-10-22T00:02:00.100Z',
  ])
})

test('a Settings change during a pending wake runs the newly saved job without waiting for a timer', async t => {
  const h = await host(t)
  const gate = Promise.withResolvers()
  h.readGate = gate
  await h.owner.parallel('settings/schedule', { source: 'schedule' })
  await until(() => h.readGate === undefined, 'wake captured the old Settings document')
  const job = (await h.call('save', { job: { ...seed, schedule: { every_minutes: 1 } } })).job
  assert.equal(h.inputs.length, 0)
  gate.resolve()
  await until(() => h.runs().some(run => run.job_id === job.id && run.state === 'completed'),
    'the queued Settings change runs without a clock advance')
})

test('deleting another job during retention schedules cleanup for both owned Sessions', async t => {
  const h = await host(t)
  const first = (await h.call('save', { job: seed })).job
  const second = (await h.call('save', { job: { ...seed, name: 'second' } })).job
  for (const job of [first, second]) {
    const accepted = await h.call('run_now', { job_id: job.id })
    await until(() => h.runs().some(run => run.run_id === accepted.run_id && run.state === 'completed'), 'manual run finishes')
  }
  await flush()
  h.deleteGate = Promise.withResolvers()
  const removingFirst = h.call('delete', { job_id: first.id })
  await h.advance(1000)
  await until(() => h.deletions.length === 1, 'first cleanup has captured the surviving job reference')
  await h.call('delete', { job_id: second.id })
  h.deleteGate.resolve()
  await removingFirst
  await h.advance(1000)
  await until(() => h.deletions.length === 2 && h.runs().length === 0, 'the second deletion also triggers retention')
  assert.deepEqual(h.deletions.map(request => request.sessionId).sort(), ['s1', 's2'])
})

test('a newly saved expired one-off is recorded as missed and disabled without launch', async t => {
  const h = await host(t)
  const at = new Date(Date.now() - 60_000).toISOString()
  const job = (await h.call('save', { job: { ...seed, schedule: { at } } })).job
  await until(() => h.runs().some(run => run.job_id === job.id && run.state === 'missed'), 'expired instant is recorded')
  await h.advance(1000)
  await until(() => !h.document.jobs.find(value => value.id === job.id).enabled, 'the recorded one-off disables')
  assert.equal(h.launches.length, 0)
  assert.equal(h.inputs.length, 0)
})

test('raw cron and deadline errors do not prevent another job running on wake or the next minute', async t => {
  const h = await host(t)
  const good = (await h.call('save', { job: seed })).job
  h.document.jobs = [{ ...good, id: 'bad-cron', schedule: { cron: '99 * * * *' } },
    { ...good, id: 'bad-deadline', schedule: { every_minutes: 1 }, max_run_minutes: 5_000_000_000 },
    { ...good, schedule: { every_minutes: 1 } }]
  await h.owner.parallel('settings/schedule', { source: 'schedule' })
  await until(() => h.runs().some(run => run.job_id === good.id && run.state === 'completed'), 'valid job after invalid cron completes')
  const listed = await h.call('list')
  assert.equal(listed.jobs[0].id, 'bad-cron')
  assert.equal(listed.jobs[0].next_window, null)
  assert.match(listed.jobs[0].schedule_error, /range|minute|constraint/i)
  const visible = JSON.parse((await h.tool({ action: 'list' })).content)
  assert.ok(visible.jobs[0].schedule_error)
  assert.equal(h.launches.length, 1)
  await h.advance(60_000)
  await until(() => h.runs().filter(run => run.job_id === good.id && run.state === 'completed').length === 2,
    'the minute tick also isolates an invalid raw deadline')
  assert.equal(h.launches.length, 2)
})

test('an applied unconsumed input stays pending past its deadline without repeated withdrawal', async t => {
  const h = await host(t, { mode: 'applied' })
  const job = (await h.call('save', { job: { ...seed, max_run_minutes: 1 } })).job
  const accepted = await h.call('run_now', { job_id: job.id })
  await until(() => h.runs()[0]?.input_event_id, 'applied input recorded')
  await h.advance(60_000)
  for (let second = 0; second < 10; second++) await h.advance(1000)
  assert.equal(h.runs()[0].state, 'submitted')
  assert.deepEqual(h.withdrawals, [])
  assert.deepEqual(h.stops, [])
  const run = h.runs()[0]
  h.append(run.session_id, 'input', 'consumed', { input_id: run.input_event_id })
  h.append(run.session_id, 'turn', 'end', { status: 'completed' })
  await h.advance(1000)
  await until(() => h.runs()[0].state === 'completed', 'the actual terminal fact settles the original run')
  assert.equal(h.runs()[0].run_id, accepted.run_id)
})

test('a tool update from bound reuse to create clears only the old binding', async t => {
  const h = await host(t)
  const old = (await h.call('save', { job: { ...seed, mode: 'reuse', session_id: 'external' } })).job
  const reply = await h.tool({ action: 'update', job_id: old.id, job: { mode: 'create' } })
  assert.equal(reply.isError, undefined)
  assert.equal(h.document.jobs[0].mode, 'create')
  assert.equal('session_id' in h.document.jobs[0], false)
  assert.deepEqual(JSON.parse(reply.content).transition.from.origin, 'user')
  assert.deepEqual(JSON.parse(reply.content).transition.to.origin, { tool: { session_id: 'a', branch_id: 'main' } })
  assert.equal(h.document.jobs[0].revision, old.revision + 1)
})

test('first reuse persists its launch and waits for the binding acknowledgement before input', async t => {
  const h = await host(t)
  const job = (await h.call('save', { job: { ...seed, mode: 'reuse' } })).job
  const gate = Promise.withResolvers()
  h.updateGate = gate
  const accepted = await h.call('run_now', { job_id: job.id })
  await until(() => h.updates.some(write => write.document.jobs.some(value => value.id === job.id && value.session_id === 's1')),
    'the binding write is pending')
  assert.equal(h.inputs.length, 0)
  const pending = h.runs().find(run => run.run_id === accepted.run_id)
  assert.equal(pending.session_id, 's1')
  assert.equal(pending.session_origin, 'launch')
  assert.deepEqual(pending.policy, h.launches[0].policy)
  gate.resolve()
  await until(() => h.runs().find(run => run.run_id === accepted.run_id)?.state === 'completed', 'confirmed binding permits input')
  assert.equal(h.document.jobs[0].session_id, 's1')
  assert.equal(h.document.jobs[0].revision, job.revision + 1)
  assert.equal(h.inputs.length, 1)
  assert.equal(h.inputs[0].sessionId, 's1')
  assert.equal(h.inputs[0].content[0].text, job.prompt)
  const next = await h.call('run_now', { job_id: job.id })
  await until(() => h.runs().find(run => run.run_id === next.run_id)?.state === 'completed', 'next run uses the committed binding')
  assert.equal(h.launches.length, 1)
})

test('a deadline reached during first-reuse binding keeps the confirmed Session without input', async t => {
  const h = await host(t)
  const job = (await h.call('save', { job: { ...seed, mode: 'reuse', max_run_minutes: 1 } })).job
  const gate = Promise.withResolvers()
  h.updateGate = gate
  const accepted = await h.call('run_now', { job_id: job.id })
  await until(() => h.updateGate === undefined, 'the binding write is pending')
  t.mock.timers.setTime(Date.now() + 60_000)
  gate.resolve()
  await until(() => h.runs().find(run => run.run_id === accepted.run_id)?.state === 'timed_out', 'the existing pre-input check enforces the deadline')
  const run = h.runs().find(run => run.run_id === accepted.run_id)
  assert.equal(run.detail, 'deadline_before_input')
  assert.equal(run.session_origin, 'launch')
  assert.equal(run.session_id, 's1')
  assert.equal(h.document.jobs[0].session_id, 's1')
  assert.equal(h.inputs.length, 0)
  assert.equal(h.launches.length, 1)
  assert.equal(h.withdrawals.length, 0)
  assert.equal(h.stops.length, 0)
})

test('unrelated edits preserve the first reuse window through one or two definitive conflicts', async t => {
  for (const conflicts of [1, 2]) await t.test(`${conflicts} conflicts`, async t => {
    const h = await host(t)
    const scheduled = conflicts === 2
    const at = new Date(Date.now() + 1000).toISOString()
    const job = (await h.call('save', { job: { ...seed, mode: 'reuse', schedule: { at: scheduled ? at : seed.schedule.at } } })).job
    const other = (await h.call('save', { job: { ...seed, name: 'other' } })).job
    const bindings = () => h.updates.filter(write => write.document.jobs.some(value => value.id === job.id && value.session_id === 's1'))
    const gates = [Promise.withResolvers(), Promise.withResolvers()]
    h.updateGate = gates[0]
    let accepted
    if (scheduled) {
      await h.advance(1000)
      await until(() => h.runs().some(run => run.job_id === job.id), 'scheduled first window starts')
      accepted = h.runs().find(run => run.job_id === job.id)
    } else accepted = await h.call('run_now', { job_id: job.id })
    await until(() => bindings().length === 1, 'first binding is pending')
    await h.call('save', { job: { id: other.id, name: 'unrelated edit 1' } })
    if (conflicts === 2) h.updateGate = gates[1]
    gates[0].resolve()
    if (conflicts === 2) {
      await until(() => bindings().length === 2, 'one revalidated binding retry is pending')
      await h.call('save', { job: { id: other.id, name: 'unrelated edit 2' } })
      assert.equal(h.inputs.length, 0)
      assert.equal(h.document.jobs.find(value => value.id === job.id).enabled, true)
      gates[1].resolve()
    }
    await until(() => h.runs().find(run => run.run_id === accepted.run_id)?.state === 'completed', 'the first window executes once')
    const result = h.runs().find(run => run.run_id === accepted.run_id)
    assert.equal(bindings().length, 2)
    assert.equal(h.launches.length, 1)
    assert.equal(h.inputs.length, 1)
    assert.equal(h.inputs[0].sessionId, 's1')
    assert.equal(h.document.jobs.find(value => value.id === other.id).name, `unrelated edit ${conflicts}`)
    const bound = h.document.jobs.find(value => value.id === job.id)
    assert.equal(bound.session_id, conflicts === 1 ? 's1' : undefined)
    assert.equal(result.detail, conflicts === 2 ? 'binding_conflict; completed' : 'completed')
    if (scheduled) {
      await h.advance(1000)
      await until(() => !h.document.jobs.find(value => value.id === job.id).enabled, 'ordinary settlement disables the executed one-off')
      assert.equal(h.inputs.length, 1)
    }
  })
})

test('definitively rejected first-reuse bindings fail without input or retry', async t => {
  for (const code of ['invalid_document', 'user_only_field']) await t.test(code, async t => {
    const h = await host(t)
    const saved = await h.tool({ action: 'create', job: { ...seed, mode: 'reuse' } })
    const job = saved.structuredContent.job
    h.writeFailure = code
    const before = h.updates.length
    const accepted = await h.call('run_now', { job_id: job.id })
    await until(() => h.runs().find(run => run.run_id === accepted.run_id)?.state === 'failed', 'binding rejection fails the run')
    await until(() => h.inputs.some(input => input.submissionId === `scheduler-report:${accepted.run_id}`), 'failed binding reports once')
    assert.equal(h.runs().find(run => run.run_id === accepted.run_id).detail, `binding_rejected: ${code}`)
    assert.equal(h.updates.length, before + 1)
    assert(h.inputs.every(input => input.mode === 'steer'))
    assert.equal(h.document.jobs[0].session_id, undefined)
  })
})

test('possibly committed binding replies skip input and later runs learn the actual Settings binding', async t => {
  for (const code of ['unsynced', 'failed', 'closed', 'cancelled', 'bridge']) await t.test(code, async t => {
    const h = await host(t)
    const job = (await h.call('save', { job: { ...seed, mode: 'reuse' } })).job
    h.afterWriteFailure = code
    const before = h.updates.length
    const accepted = await h.call('run_now', { job_id: job.id })
    await until(() => h.runs().find(run => run.run_id === accepted.run_id)?.state === 'skipped', 'uncertain binding skips input')
    assert.equal(h.runs().find(run => run.run_id === accepted.run_id).detail, 'binding_unconfirmed')
    assert.equal(h.inputs.length, 0)
    assert.equal(h.updates.length, before + 1)
    assert.equal(h.document.jobs[0].session_id, 's1')
    h.afterWriteFailure = undefined
    const next = await h.call('run_now', { job_id: job.id })
    await until(() => h.runs().find(run => run.run_id === next.run_id)?.state === 'completed', 'the next run reads the actual binding')
    assert.equal(h.launches.length, 1)
    assert.equal(h.inputs.length, 1)
    assert.equal(h.inputs[0].sessionId, 's1')
  })
})

test('a selected-job edit or Settings reread failure prevents first-reuse input', async t => {
  for (const change of ['changed', 'disabled', 'bound', 'create', 'read failure']) await t.test(change, async t => {
    const h = await host(t)
    const job = (await h.call('save', { job: { ...seed, mode: 'reuse' } })).job
    const gate = Promise.withResolvers()
    h.updateGate = gate
    const accepted = await h.call('run_now', { job_id: job.id })
    await until(() => h.updateGate === undefined, 'the initial binding write is pending')
    const current = h.document.jobs[0]
    if (change === 'changed') { current.name = 'replacement'; current.revision++ }
    if (change === 'disabled') { current.enabled = false; delete current.enabled_at; current.revision++ }
    if (change === 'bound') current.session_id = 'external'
    if (change === 'create') current.mode = 'create'
    h.revision++
    if (change === 'read failure') h.readFailure = true
    gate.resolve()
    await until(() => h.runs().find(run => run.run_id === accepted.run_id)?.state === 'skipped', 'changed authorization or unreadable Settings skips input')
    const run = h.runs().find(run => run.run_id === accepted.run_id)
    assert.equal(run.detail, change === 'read failure' ? 'settings_read_failed' : change === 'disabled' ? 'disabled' : 'changed')
    assert.equal(h.inputs.length, 0)
    assert.equal(h.launches.length, 1)
    assert.equal(h.document.jobs[0].session_id, change === 'bound' ? 'external' : undefined)
  })
})

test('idle scheduled jobs do not read Settings or the run store every second', async t => {
  const h = await host(t)
  await h.call('save', { job: seed })
  await flush()
  const settings = h.settingsReads, store = h.storeReads
  for (let second = 0; second < 10; second++) await h.advance(1000)
  assert.equal(h.settingsReads, settings)
  assert.equal(h.storeReads, store)
})

test('a recent latest window still fires after an older outage gap', async t => {
  const h = await host(t)
  const job = (await h.call('save', { job: { ...seed, schedule: { every_minutes: 1 } } })).job
  await until(() => h.runs()[0]?.state === 'completed', 'the anchor runs')
  await h.advance(10 * 60_000 + 30_000)
  await until(() => h.runs().length === 2 && h.runs()[1].state === 'completed', 'the recent window runs')
  assert.equal(h.runs()[1].window, '2026-10-22T00:10:00.000Z')
  assert.equal(h.launches.length, 2)
  assert.ok(h.runs().every(run => run.job_id === job.id))
})

test('a deadline stop refused during reload is retried before declaring a timeout', async t => {
  const h = await host(t, { mode: 'running', stopReloads: 1 })
  const job = (await h.call('save', { job: { ...seed, max_run_minutes: 1 } })).job
  await h.call('run_now', { job_id: job.id })
  await until(() => h.runs()[0]?.state === 'running', 'the input is consumed')
  await h.advance(60_000)
  assert.equal(h.stops.length, 1)
  assert.equal(h.runs()[0].state, 'running')
  await h.advance(1000)
  assert.equal(h.stops.length, 2)
  await h.advance(1000)
  await until(() => h.runs()[0].state === 'timed_out', 'the accepted stop settles')
  assert.equal(h.stops.length, 2)
})

test('an uncertain lookup read retries without losing or resubmitting accepted input', async t => {
  const h = await host(t, { inputReplyLost: true, lookupFailures: 1 })
  const job = (await h.call('save', { job: seed })).job
  await h.call('run_now', { job_id: job.id })
  await until(() => h.lookupFailures === 0, 'the first lookup read failed')
  assert.equal(h.runs()[0].state, 'launching')
  await h.advance(1000)
  await until(() => h.runs()[0].state === 'completed', 'the next lookup finds the receipt')
  assert.equal(h.inputs.length, 1)
  assert.equal(h.launches.length, 1)
})

test('a report guidance with a line break is refused when the plugin loads', async t => {
  const h = await host(t)
  for (const report_guidance of ['Read the run history.\nThen decide.', 'Read the run history.\rThen decide.']) {
    let unexpected
    try {
      await assert.rejects(async () => {
        unexpected = await load(processPlugin, { services: h.services, config: { report_guidance } })
      }, /report_guidance must be one line: the entry names the report's last line as guidance/)
    } finally {
      // Only reached when the load was not refused: unload it and move on.
      void unexpected?.unload().catch(() => {})
    }
  }
})

test('the report guidance is the report body\'s own last line', async t => {
  const h = await host(t)
  const job = JSON.parse((await h.tool({ action: 'create', job: seed })).content).job
  await h.call('run_now', { job_id: job.id })
  await until(() => h.inputs.some(input => input.mode === 'steer'), 'report published')
  const text = h.inputs.find(input => input.mode === 'steer').rawBlocks[0].text
  const body = text.slice(text.indexOf('>') + 1, -'</dim-scheduler>'.length)
  const lines = body.split('\n')
  assert.equal(lines.at(-1), 'Inspect schedule.history for this job.')
  assert.match(lines.at(-2), /^Scheduled job .*: completed \(.*\)\. Session .*\.$/)
})
