import { definePlugin } from '@arcships/rutis'
import type { ManagementMethods, MethodReply, SqliteStorage, Store } from '../../npm/plugin.js'
import { writeFile, rename, rm } from 'node:fs/promises'
import assert from 'node:assert/strict'

export default definePlugin({
  name: 'StoreFixture',
  provides: {},
  inject: ['managementMethods', 'store', 'sqliteStorage'],
  async apply(ctx) {
    const store = ctx.use<Store>('store')
    const sqlite = ctx.use<SqliteStorage>('sqliteStorage')
    let release: (() => void) | undefined
    await ctx.use<ManagementMethods>('managementMethods').register(
      'storefixture',
      ['status', 'commit_round', 'timeline_directory', 'reserve_file', 'file_ownership', 'selection', 'agent_records', 'read_user', 'import_user', 'replace_user', 'report', 'create', 'cycle', 'remove', 'refuse', 'unregistered_refuse', 'unregistered_remove', 'release', 'invalid_id'].map(name => ({ name: `storefixture.${name}`, mutating: true, interruption: 'report', progress: false })),
      async (call: { method: string; params: any }): Promise<MethodReply> => {
        const params = call.params
        if (call.method === 'storefixture.status') return { kind: 'value', value: {} }
        if (call.method === 'storefixture.timeline_directory') {
          const { sessionId, config } = params
          if (config) await sqlite.createSession({ sessionId, config })
          const key = { sessionId, branchId: 'main' }
          const { sequence } = await sqlite.head(key)
          assert.deepEqual(await sqlite.batchEnd({ ...key, sequence }), {})
          assert.deepEqual(await sqlite.batchEndAt({ ...key, sequence }), { sequence })
          return { kind: 'value', value: {
            ...await sqlite.sessionConfig({ sessionId }),
            ...await sqlite.branches({ sessionId }),
            sequence,
          } }
        }
        if (call.method === 'storefixture.commit_round') return { kind: 'value', value: await sqlite.commitTimelineRound(params) }
        if (call.method === 'storefixture.selection') {
          const scope = 'workspace:/fixture'
          const selection = {providerId:'fixture', modelId:'first'}
          assert.equal(await sqlite.readSelection({scope}), null)
          assert.equal(await sqlite.readPreferences(selection), null)
          const saved = await sqlite.writeSelection({scope, selection, expectedVersion:null})
          assert.deepEqual(saved, {provider_id:'fixture', model_id:'first', version:1})
          await assert.rejects(() => sqlite.writeSelection({scope, selection, expectedVersion:null}),
            (error: unknown) => error instanceof Error && 'code' in error && error.code === 'conflict')
          return {kind:'value', value: await sqlite.readSelection({scope})}
        }
        if (call.method === 'storefixture.reserve_file') {
          return { kind: 'value', value: await sqlite.reserveFile(params.file) }
        }
        if (call.method === 'storefixture.file_ownership') {
          const { file, path } = params
          const { sessionId } = file
          assert.equal(await sqlite.commitFile({ ...file, generation: 'other' }), 'superseded')
          assert.equal(await sqlite.commitFile(file), 'owned')
          assert.equal(await sqlite.ownsFile({ sessionId, fileId: file.fileId }), true)
          const abandoned = { ...file, fileId: 'fixture-abandoned' }
          assert.equal(await sqlite.reserveFile(abandoned), true)
          assert.equal(await sqlite.abandonFile({ fileId: abandoned.fileId, generation: file.generation }), 1)
          assert.equal(await sqlite.registerOutput({ sessionId: 'missing', path }), false)
          assert.equal(await sqlite.ownsFile({ sessionId, fileId: 'absent' }), false)
          assert.equal(await sqlite.ownsOutput({ sessionId, path }), false)
          assert.equal(await sqlite.registerOutput({ sessionId, path }), true)
          assert.equal(await sqlite.ownsOutput({ sessionId, path }), true)
          assert.ok((await sqlite.ownedPaths({ sessionId }))[1].includes(path))
          return { kind: 'value', value: {} }
        }
        if (call.method === 'storefixture.agent_records') {
          const { agent, leader } = params
          const profile = { name: 'Fixture', description: '', systemPrompt: '' }
          await sqlite.save({id: agent, profile, default: false, owner: leader})
          assert.deepEqual((await sqlite.agents({})).find(row => row.id === agent), {id: agent, profile: {Ok: profile}, default: false, owner: leader})
          assert.equal(await sqlite.agentOwner({agent}), leader)
          assert.deepEqual(await sqlite.ownedAgents({owner: leader}), [agent])
          assert.deepEqual(await sqlite.memberSessions({agent}), [])
          assert.deepEqual(await sqlite.ledMembers({leader}), [])
          assert.equal((await sqlite.members({})).some(([, id]) => id === leader), false)
          assert.ok((await sqlite.unfinishedOwned({})).incomplete.includes(agent))
          const owner = (await sqlite.owners({})).find(row => row.sessionId === leader)
          assert.ok(owner?.owner)
          await sqlite.delete({id:agent})
          assert.equal(await sqlite.agentOwner({agent}), null)
          return {kind: 'value', value: {leaderAgent: owner.owner[0]}}
        }
        if (call.method === 'storefixture.replace_user') {
          const source = 'fixture-import'
          const rejected = { kind: 'reject' as const, field: '/protected' }
          for (const [expectedVersion, code] of [[0, 'conflict'], [1, 'user_only_field']] as const) {
            await assert.rejects(() => sqlite.replaceUser({ source, expectedVersion, decision: rejected }),
              (error: unknown) => error instanceof Error && 'code' in error && error.code === code)
          }
          return { kind: 'value', value: await sqlite.replaceUser({source, expectedVersion: 1, decision: {kind: 'write', document: '{"value":3}'}}) }
        }
        if (call.method === 'storefixture.import_user') return { kind: 'value', value: await sqlite.importUserIfAbsent(params) }
        if (call.method === 'storefixture.read_user') return { kind: 'value', value: await sqlite.readUser(params) }
        if (call.method === 'storefixture.report') return { kind: 'value', value: await sqlite.storageReport({}) }
        if (call.method === 'storefixture.create') {
          if (!params.create) throw new Error('missing creation request')
          const result = await store.createOwnedSession(params.create)
          return { kind: 'value', value: result }
        }
        if (call.method === 'storefixture.invalid_id') {
          const errors: { code: string; message: string }[] = []
          for (const operation of [
            () => store.createOwnedSession(params.create),
            () => store.fork({ sourceSessionId: 'wire-source', sessionId: params.create.sessionId, at: 0, configText: '{}' }),
            () => store.delete({ sessionId: params.create.sessionId }),
          ]) {
            await assert.rejects(operation, (error: unknown) => {
              assert.ok(error instanceof Error && 'code' in error)
              assert.equal(error.code, 'invalid')
              assert.ok(typeof error.code === 'string')
              errors.push({ code: error.code, message: error.message })
              return true
            })
          }
          return { kind: 'value', value: { errors } }
        }
        if (call.method === 'storefixture.unregistered_refuse') {
          let called = false
          try {
            await store.removeUnregisteredDirectory({ root: params.root }, async () => {
              called = true
            })
            throw new Error('removal unexpectedly succeeded')
          } catch (error) {
            if (!(error instanceof Error) || !('code' in error) || typeof error.code !== 'string') throw error
            return { kind: 'value', value: { code: error.code, message: error.message, called } }
          }
        }
        if (call.method === 'storefixture.unregistered_remove') {
          const removed = await store.removeUnregisteredDirectory({ root: params.root }, async path => {
            const gate = new Promise<void>(resolve => { release = resolve })
            await writeFile(`${params.ready}.pending`, path)
            await rename(`${params.ready}.pending`, params.ready)
            await gate
            await rm(path, { recursive: true })
          })
          return { kind: 'value', value: removed }
        }
        if (call.method === 'storefixture.release') { release?.(); return { kind: 'value', value: {} } }
        if (call.method === 'storefixture.refuse') {
          try {
            await store.removeWorktree(params.directory, async () => {
              throw Object.assign(new Error('fixture refusal'), { code: 'invalid' })
            })
            throw new Error('removal unexpectedly succeeded')
          } catch (error) {
            if (!(error instanceof Error) || !('code' in error) || typeof error.code !== 'string') throw error
            return { kind: 'value', value: { code: error.code, message: error.message } }
          }
        }
        if (call.method === 'storefixture.remove') {
          const removed = await store.removeWorktree(params.directory, async path => {
            assert.deepEqual(await store.worktreeUsages(params.directory), { usages: [] })
            const gate = new Promise<void>(resolve => { release = resolve })
            await writeFile(`${params.ready}.pending`, path)
            await rename(`${params.ready}.pending`, params.ready)
            await gate
            await rm(path, { recursive: true })
          })
          return { kind: 'value', value: removed }
        }
        const { sessionId, forkId, grant } = params
        await store.addGrant({ sessionId, path: grant, kind: 'dir', mode: 'write' })
        await store.addGrant({ sessionId, path: grant, kind: 'dir', mode: 'read' })
        const grants = await store.grants({ sessionId })
        await store.setSessionLanguage({ sessionId, language: null })
        const cleared = await store.sessionConfig({ sessionId })
        assert.ok(cleared.config)
        assert.equal(cleared.config.language, undefined)
        const directories = await store.worktreeDirectories({})
        assert.equal(directories.directories.length, 1)
        const directory = directories.directories[0]
        assert.deepEqual(await store.sessionsUnderWorkspace({ root: directory.root }), { sessionIds: [sessionId] })
        assert.deepEqual(await store.worktreeUsages({ repository: directory.repository, root: directory.root }), {
          usages: [{ sessionId, subtree: directory.root }],
        })
        await assert.rejects(() => store.removeWorktreeDirectory({ repository: directory.repository, root: directory.root }),
          (error: unknown) => error instanceof Error && 'code' in error && error.code === 'invalid')
        assert.deepEqual(await store.removeWorktreeDirectory({ repository: directory.repository, root: `${directory.root}/absent` }), { removed: false })
        await assert.rejects(() => Reflect.apply(store.worktreeDirectories, store, [{ unexpected: true }]))
        await assert.rejects(() => Reflect.apply(store.sessionConfig, store, [[sessionId]]), /request must be an object/)
        await assert.rejects(() => Reflect.apply(store.worktreeDirectories, store, [[]]), /request must be an object/)
        await assert.rejects(() => Reflect.apply(store.catalog, store, [[{}, null, 3, [], []]]), /request must be an object/)
        await assert.rejects(() => Reflect.apply(store.removeWorktree, store, [
          [directory.repository, directory.root], () => { throw new Error('invalid request ran its callback') },
        ]), /request must be an object/)
        await store.setSessionLanguage({ sessionId, language: 'zh-CN' })
        const config = await store.sessionConfigText({ sessionId })
        if (config.configText === null) throw new Error('missing source')
        await store.fork({ sourceSessionId: sessionId, sessionId: forkId, at: 0, configText: config.configText })
        const fork = await store.hasRecords({ sessionId: forkId })
        const removed = await store.delete({ sessionId: forkId })
        const absent = await store.hasRecords({ sessionId: forkId })
        const origin = await store.creationRecord({ sessionId })
        const owner = await store.owner({ sessionId })
        const query = await store.query({ scope: {
          key: { sessionId, branchId: 'main' }, through: 0,
          segments: [{ branchId: 'main', from: 1, through: 0, storedThrough: 0, tail: [] }],
        }, query: { select: { kind: 'count' } } })
        assert.deepEqual(query, { result: { kind: 'count', count: 0 }, next: null })
        if (owner.agentId === null) throw new Error('missing source owner')
        const catalog = await store.catalog({ request: {}, version: 3, hosts: [owner.agentId], closed: [] })
        assert.deepEqual(catalog.sessions.map(session => session.sessionId), [sessionId])

        await store.removeGrant({ sessionId, path: grant })
        let missing: { code: string; message: string } | null = null
        try {
          await store.setSessionLanguage({ sessionId: 'wire-missing', language: null })
        } catch (error) {
          if (!(error instanceof Error) || !('code' in error) || typeof error.code !== 'string') throw error
          missing = { code: error.code, message: error.message }
        }
        return { kind: 'value', value: { grants, config, fork, removed, absent, origin, owner, missing, catalog } }
      },
    )
  },
})
