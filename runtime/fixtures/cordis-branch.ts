import type { Context } from '@deepseek-ai/cordis'

export const name = 'cordis-branch-fixture'
export const inject = ['dispatcher', 'timelineIdentity', 'timelineTree']

export async function apply(ctx: Context) {
  const { sessionId, branchId } = await ctx.get('timelineIdentity').get()
  let written = false
  await ctx.get('dispatcher').listen('cordis-branch-fixture', ['turn/end'], async () => {
    if (written) return
    written = true
    await ctx.get('timelineTree').input(branchId, {
      blocks: [{ type: 'raw', text: `<cordis-marker session="${sessionId}" branch="${branchId}"/>` }],
      mode: 'queue',
    })
  })
}
