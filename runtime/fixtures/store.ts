import { definePlugin } from '@arcships/rutis'
import type { ManagementMethods, MethodReply, Store } from '../../npm/plugin.js'
import { writeFile, rename, rm } from 'node:fs/promises'
import assert from 'node:assert/strict'

export default definePlugin({
  name: 'StoreFixture',
  provides: {},
  inject: ['managementMethods', 'store'],
  async apply(ctx) {
    const store = ctx.use<Store>('store')
    let release: (() => void) | undefined
    await ctx.use<ManagementMethods>('managementMethods').register(
      'storefixture',
      ['status', 'create', 'cycle', 'remove', 'refuse', 'release', 'invalid_id'].map(name => ({ name: `storefixture.${name}`, mutating: true, interruption: 'report', progress: false })),
      async (call: { method: string; params: any }): Promise<MethodReply> => {
        const params = call.params
        if (call.method === 'storefixture.status') return { kind: 'value', value: {} }
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
