import { definePlugin } from '@arcships/rutis'
import type { ManagedWorktrees, ManagementMethods } from '../../npm/plugin.js'
import assert from 'node:assert/strict'

export default definePlugin({
  name: 'ManagedWorktreesFixture',
  provides: {},
  inject: ['managedWorktrees', 'managementMethods'],
  async apply(ctx) {
    const owner = ctx.use<ManagedWorktrees>('managedWorktrees')
    await ctx.use<ManagementMethods>('managementMethods').register(
      'worktreefixture',
      [{ name: 'worktreefixture.exercise', mutating: true, interruption: 'drain', progress: false }],
      async ({ params }) => {
        assert.ok(params && typeof params === 'object' && 'source' in params && typeof params.source === 'string')
        const created = await owner.create({ source: params.source, branch: 'wire-worktree' })
        const listed = await owner.list({})
        const selected = await owner.pruneSelection({ keep: 0, force: false })
        assert.deepEqual(listed.map(row => row.taskId), [created.taskId])
        assert.deepEqual(selected.candidates, [])
        assert.deepEqual(selected.kept.map(row => row.worktree.taskId), [created.taskId])
        await assert.rejects(() => owner.remove({ taskId: 'invalid', force: false }), { code: 'worktree_invalid' })
        await assert.rejects(owner.discard({ ...created, branch: 'another-branch' }), { code: 'worktree_invalid' })
        assert.deepEqual((await owner.list({})).map(row => row.taskId), [created.taskId])
        await owner.discard(created)
        return { kind: 'value', value: { created, remaining: await owner.list({}) } }
      },
    )
  },
})
