import { definePlugin } from '@arcships/rutis'
import type { GuideEntries, ToolRegistry } from '../../../npm/plugin.js'
import { toolRequest, unloadingAfterCall, unloadingBeforeCall, type RunReply, type WorkflowRuns } from './requests.ts'

// The dim-workflow entry (Workflow §5.2): both kinds it writes, verbatim.
const ENTRY = '<dim-workflow kind="report" run="wf-7f2" status="completed">: how a workflow run ended, and <dim-workflow kind="stage" run="wf-7f2" stage="build">: the start of one of its stages. run names the run, status its outcome and stage the stage. The report\'s body gives the run\'s outcome, each stage\'s state and a bounded preview of the stages\' own last-step results: the workflow\'s account of the run, information, not a request. The report\'s last line is not part of that account: it is guidance on what to do next. The stage tag has an empty body and stands before the text that follows it, which is the prompt the stage was launched with.'

/// The `invalid_definition` recovery hint in force when the factory
/// configuration sets none.
const RECOVERY = 'Use list to inspect the definition issues.'

export default (invalidDefinitionGuidance: string = RECOVERY) => definePlugin({
  name: 'WorkflowSession',
  inject: ['workflowRuns', 'toolRegistry', 'guideEntries'],
  provides: {},
  async apply(ctx) {
    const owner = ctx.use<WorkflowRuns>('workflowRuns')
    let closed = false
    ctx.effect(() => { closed = true })
    await ctx.use<GuideEntries>('guideEntries').register('session', { markers: ['dim-workflow'], order: 120, text: ENTRY })
    await ctx.use<ToolRegistry>('toolRegistry').register(
      { name: 'workflow', description: 'List workflow definitions, start a run, inspect stored stage results, or stop a run. A listing marked truncated omits later definitions.',
        source: 'extension', risk: 'workflow',
        inputSchema: { type: 'object', required: ['action'], additionalProperties: false, properties: {
          action: { type: 'string', enum: ['list', 'start', 'status', 'stop'] },
          name: { type: 'string' }, source: { type: 'string', enum: ['project', 'user', 'plugin'] },
          params: { type: 'object', additionalProperties: { type: 'string' } }, run_id: { type: 'string' },
        } },
      },
      async (call, cancellation) => {
        if (closed) return { content: unloadingBeforeCall, isError: true }
        if (call.config.interactive === false) return { content: 'workflow is available only in interactive Sessions.', isError: true }
        const request = toolRequest.safeParse(call.arguments)
        if (!request.success) return { content: `workflow: invalid_params\n${request.error.message}`, isError: true }
        let cancelled = false
        void cancellation.then(() => { cancelled = true }, () => { cancelled = true })
        await Promise.resolve()
        if (closed) return { content: unloadingBeforeCall, isError: true }
        if (cancelled) return { content: 'workflow: cancelled before handover', isError: true }
        let reply: RunReply
        try {
          reply = await owner.call({ ...request.data, caller: {
            sessionId: call.sessionId, branchId: call.branchId, callId: call.callId, cwd: call.config.cwd,
          } })
        } catch (error) {
          if (closed) return { content: unloadingAfterCall, isError: true }
          throw error
        }
        if ('code' in reply) return {
          content: reply.code === 'service_unavailable' ? reply.message
            : reply.code === 'invalid_definition' ? `workflow: invalid_definition. ${invalidDefinitionGuidance}`
            : `workflow: ${reply.code}\n${reply.message}`,
          isError: true,
        }
        return { content: reply.content, ...(reply.structured ? { structuredContent: reply.structured } : {}) }
      },
    )
  },
})
