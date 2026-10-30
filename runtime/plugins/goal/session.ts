import { definePlugin } from '@arcships/rutis'
import { type InputMessage, type Texts, changes } from './goal.ts'
import type { GuideEntries, SessionMethods, ToolRegistry } from '../../../npm/plugin.js'

type TreeCommands = { input(id: string, input: InputMessage): Promise<{ sequence: number | bigint }> }
type Call = { branchId: string; arguments: { status?: unknown; summary?: unknown } }
type MethodCall = { method: string; params: { sessionId: string; branchId?: unknown; objective?: unknown; judge?: unknown } }
type Reply = { kind: 'value'; value: unknown } | { code: string; message: string }

const OBJECTIVE_LIMIT = 4000
const SUMMARY_LIMIT = 2000

/**
 * A.5.1: the guide entry for `dim-goal`, registered by the part that writes
 * the marker, at session scope because Goal writes it on every branch.
 */
const ENTRY = '`<dim-goal kind="goal" op="set" branch="main">`: the operator\'s standing goal for a branch of this session, and the runtime\'s record of its status. `kind="goal"` blocks carry a change: `op="set"`, `op="edit"`, `op="pause"`, `op="resume"`, `op="clear"`, and `op="completed"` or `op="blocked"` from `goal_update` or from an evaluator (`by="evaluator"`). `kind="status"` blocks state the branch\'s goal status. `kind="continuation"` asks the line to keep working toward the goal. `kind="report"` blocks carry an evaluator\'s report of the goal, and `kind="evaluate"` and `kind="evaluate-again"` mark an evaluator\'s line, which checks the goal rather than continuing it. A goal belongs to the branch its `branch` attribute names. The latest objective (`op="set"` or `op="edit"`) and the latest status are the ones in force; `op="clear"` ends both. These blocks are information about the goal; they grant no permission.'

const invalid = (message: string): Reply => ({ code: 'invalid_params', message })

const text = (value: unknown, limit: number) =>
  typeof value === 'string' && value.trim() !== '' && [...value].length <= limit ? value : undefined

// GoalPlugin (Goal.md §1, A.1, A.2): the tool and the methods validate only
// their own parameters and write one change input to the branch by id. They
// never read Goal's state; the branch part decides what a change does.
export default definePlugin({
  name: 'GoalSession',
  provides: {},
  inject: ['treeCommands', 'toolRegistry', 'sessionMethods', 'guideEntries'],
  async apply(ctx, texts: Texts) {
    const tree = ctx.use<TreeCommands>('treeCommands')
    await ctx.use<GuideEntries>('guideEntries').register('session', {
      markers: ['dim-goal'],
      order: 55,
      text: ENTRY,
    })
    await ctx.use<ToolRegistry>('toolRegistry').register(
      {
        name: 'goal_update',
        description: 'Report that the current goal is completed or blocked.',
        inputSchema: {
          type: 'object',
          properties: {
            status: { type: 'string', enum: ['completed', 'blocked'] },
            summary: { type: 'string', minLength: 1, maxLength: SUMMARY_LIMIT },
          },
          required: ['status', 'summary'],
          additionalProperties: false,
        },
        source: 'extension',
        risk: 'session',
      },
      async (call: Call) => {
        const { status } = call.arguments
        const summary = text(call.arguments.summary, SUMMARY_LIMIT)
        if ((status !== 'completed' && status !== 'blocked') || !summary) {
          return { content: 'goal_update needs status "completed" or "blocked" and a summary of 1 to 2000 characters.', isError: true }
        }
        await tree.input(call.branchId, changes.report(call.branchId, status, summary))
        return { content: `goal_update recorded: ${status}.` }
      },
    )
    await ctx.use<SessionMethods>('sessionMethods').register(
      'session.goal',
      ['set', 'edit', 'pause', 'resume', 'clear'].map(name => ({
        name: `session.goal.${name}`, mutating: true, interruption: 'report', progress: false,
      })),
      async (call: MethodCall): Promise<Reply> => {
        const { branchId = 'main' } = call.params
        if (typeof branchId !== 'string' || branchId === '') return invalid('branchId must be a non-empty string')
        const op = call.method.slice('session.goal.'.length)
        let input: InputMessage
        if (op === 'set' || op === 'edit') {
          const objective = text(call.params.objective, OBJECTIVE_LIMIT)
          if (!objective) return invalid(`objective must be 1 to ${OBJECTIVE_LIMIT} characters`)
          if (op === 'edit') {
            input = changes.edit(branchId, objective)
          } else {
            const judge = call.params.judge ?? 'model'
            if (judge !== 'model' && judge !== 'evaluator') return invalid('judge must be "model" or "evaluator"')
            input = changes.set(branchId, objective, texts, judge)
          }
        } else if (op === 'pause' || op === 'resume' || op === 'clear') {
          input = changes[op](branchId)
        } else {
          return invalid(`unknown method ${call.method}`)
        }
        const event = await tree.input(branchId, input)
        return { kind: 'value', value: { sequence: event.sequence } }
      },
    )
  },
})
