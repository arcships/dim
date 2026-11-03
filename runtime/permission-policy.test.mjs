import assert from 'node:assert/strict'
import test from 'node:test'
import { z } from 'zod'
import { jobSchema } from './plugins/scheduler/data.ts'
import { settings } from './plugins/scheduler/settings.ts'
import { RunStore as SchedulerStore } from './plugins/scheduler/store.ts'
import { RunStore as WorkflowStore } from './plugins/workflow/store.ts'

const legacy = { preset: 'full-access', allowedTools: ['read'], disallowedTools: ['exec'], fileAccess: 'read-only', toolApproval: 'ask' }
const policy = { preset: 'full-access', allowedTools: ['read'], disallowedTools: ['exec'] }
const at = '2026-10-22T00:00:00.000Z'
const cases = [
  { Store: SchedulerStore, name: 'scheduler', version: 1, field: 'policy', record: {
    run_id: 'legacy', job_id: 'job', window: 'manual', state: 'completed', deadline_at: at,
    session_id: 'session', session_origin: 'launch', input_event_id: 'input', submission_id: 'scheduler:legacy',
    policy: legacy, created_at: at, ended_at: at, detail: 'completed',
  } },
  { Store: WorkflowStore, name: 'workflow', version: 2, field: 'policy_cap', record: {
    run_id: 'legacy', name: 'legacy', source: 'user', params: {}, cwd: '/tmp', policy_cap: legacy,
    definition: { schema: 'dim.workflow/v1', name: 'legacy', description: 'Stored run', steps: [{ stage: { id: 'one', prompt: 'done' } }] },
    started_by: 'management', created_at: at, ended_at: at, state: 'completed', log: [],
    stages: [{ id: 'one', step: 0, state: 'completed', result_text: 'done' }], report: { state: 'none' },
  } },
]

for (const { Store, name, version, field, record } of cases) {
  test(`${name} reads legacy policy fields without rewriting and drops them on the next save`, async () => {
    const path = `${name}/runs.json`
    const original = JSON.stringify({ version, runs: [record] })
    const files = new Map([[path, original]])
    const scope = { async withLock(path, work) {
      return { ok: true, value: await work({
        read: async () => ({ ok: true, value: files.get(path) ?? null }),
        replace: async text => { files.set(path, text); return { ok: true, value: null } },
      }) }
    } }
    const store = new Store(scope)
    const expected = { ...record, [field]: policy }
    assert.deepEqual(await store.read(), [expected])
    assert.equal(files.get(path), original)
    await store.update(() => {})
    assert.deepEqual(JSON.parse(files.get(path)), { version, runs: [expected] })
    for (const document of [
      { version, runs: [record], unknown: true },
      { version, runs: [{ ...record, unknown: true }] },
      { version, runs: [{ ...record, [field]: { ...legacy, preset: 'invalid' } }] },
    ]) {
      const text = JSON.stringify(document)
      files.set(path, text)
      await assert.rejects(new Store(scope).read(), error => error.code === 'invalid_data')
      assert.equal(files.get(path), text)
    }
  })
}

test('Scheduler settings accept legacy embedded policy keys and reject unknown job fields', () => {
  const job = {
    id: 'job', name: 'Job', cwd: '/tmp', prompt: 'Check', schedule: { at }, mode: 'create',
    policy_cap: legacy, created_by: 'user', enabled: false, revision: 1, max_run_minutes: 60,
  }
  const source = z.fromJSONSchema(settings.schema.properties.jobs.items.properties.policy_cap)
  source.parse(legacy)
  assert.deepEqual(jobSchema.parse(job), { ...job, policy_cap: policy })
  assert.throws(() => jobSchema.parse({ ...job, unknown: true }))
})
