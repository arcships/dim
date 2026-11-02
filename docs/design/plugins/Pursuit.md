# Pursuit

Status: Implemented ([#422](https://github.com/arcships/dim/issues/422), milestone M2; the maintainer's confirmation is recorded in [§12](#12-ready-confirmation)). §13 is an amendment decided with [#427](https://github.com/arcships/dim/issues/427) and implemented with its line end. Scope: an amendment of [Goal](Goal.md)'s Session and branch parts; no new Capability ([ADR 002](../../adr/002-plugin-scope.md)). Language: TypeScript, in Goal's package `runtime/plugins/goal` ([Goal §10](Goal.md#10-implementation)). Depends on: Goal (P5, implemented by [#199](https://github.com/arcships/dim/pull/199)), C5 ([branch TypeScript binding](../branch-typescript.md)); both delivered. Milestone M2, not part of the current plan ([roadmap](../roadmap.md) D3).

Requirement ([feature list](../../feature-list.md), Agent behavior): keep working toward an explicit objective and record its completion or blockage, **with completion judged by an evaluator instead of the model doing the work**.

**Pursuit is a judge mode of Goal.** A goal set with `judge: "evaluator"` runs exactly as any Goal goal, with Goal's continuations, budget, interrupt, pause and limit. One thing changes: when a turn of the working branch ends and Goal does not continue because the worker reported the goal completed, Goal creates an **evaluator branch**. The evaluator checks the work in one turn and ends it with a conclusion: `DONE`, `CONTINUE` with what is at least still missing and the evidence, or `BLOCKED` with what only the user can provide. When the evaluator's turn ends, Goal on the evaluator writes the conclusion back to the working branch as an input, and Goal on the working branch decides: `DONE` completes the goal (only the evaluator can), `CONTINUE` becomes one continuation, `BLOCKED` blocks it. This replaces the experimental design (#17).

## 1. Decisions

| # | Decision | Why |
| --- | --- | --- |
| P1 | A judge mode of Goal, not a separate plugin | One owner of continuation; a separate plugin would race Goal on the same `turn/end` or read Goal's state (§4 rule 7) |
| P2 | The judge is chosen per goal by `set`, recorded on the `set` block | It is the user's intent for that goal, a Timeline fact |
| P3 | Evaluate when the worker's completion report stops Goal | That is the only point where the worker decides the goal is done. A worker's `blocked` stands (it needs the user); a limit, or the user's own interrupt or pause, is not a completion and starts nothing |
| P4 | Under the evaluator judge the worker's `completed` is a request; the status stays `active` | Only the evaluator completes the goal. This is the one exception to Goal's fold rule, recorded in [Goal A16](Goal.md#appendix-e-assumptions) with this design (maintainer decision, 2026-10-22) |
| P5 | The evaluator is a child branch cut at that `turn/end` | It sees what was done and why, runs under the same policy, and is an ordinary branch created with `treeCommands.create` |
| P6 | The evaluator writes its conclusion to the working branch as input; Goal there decides | A branch reads only its own events; cross-branch communication is input by id ([branch TypeScript binding](../branch-typescript.md#subscription-rebuild-and-recovery), item 6). Callers write, owners decide |

## 2. User flow

```
 user: session.goal.set {objective: "make the CLI tests pass", judge: "evaluator"}
   └─ goal active ─▶ turn ─▶ continuation ─▶ turn ...             (Goal, unchanged)
 worker: goal_update completed "all tests pass"                  (request; still active)
 turn/end N ─▶ Goal(B) does not continue ─▶ create evaluator E at N (operation goal:evaluate:N)
 E: checks, replies "... \n CONTINUE:\n - 2 tests still fail: <output>"
 E turn/end ─▶ Goal(E) writes the conclusion to B
 Goal(B):
   ├─ DONE     → completed (by evaluator)
   ├─ CONTINUE → continuation: "list may be incomplete; assess, continue, report again"
   ├─ BLOCKED  → blocked (by evaluator)
   └─ failed   → paused (evaluation_failed)
```

Clients read it all from the working branch's events ([Goal A.3](Goal.md#a3-reading-the-goal)): the judge on the `set` block, the evaluation as a `branch/create` with operation `goal:evaluate:<N>`, the conclusion input and Goal's outcome. Under the evaluator judge a worker `completed` block is a request and does not change the status (Goal A16).

## 3. Changes to Goal

| Part | Change |
| --- | --- |
| `session.ts` | `session.goal.set` accepts `judge?: "model" \| "evaluator"` (default `model`; other values `invalid_params`) and marks the `set` objective block `judge="evaluator"` |
| `goal.ts` | The fold reads `judge` (from the `set`, kept by `edit`); under the evaluator judge a worker `completed` block is a request and leaves the status unchanged, `active` while the goal runs (the exception in Goal A16). The request is neither `set` nor `resume`, so it does not move `A`: the count stays the continuations after `max(A, last_operator)` ([Goal §2](Goal.md#2-data-and-state)). Goal's `decided` also counts the `branch/create` for `goal:evaluate:N` as `N`'s decision. The decisions and texts below |
| `branch.ts` | Adds the Dispatcher keys `branch/create` and `branch/create-failed`; creates the evaluator; on an evaluator branch, takes the evaluator role (§4.2) |

No new service, Settings field or event type, and no new injection beyond Goal's `sessionLifecycle` (§13). The evaluator branch is an ordinary host-sourced child; its first Queue input is the prompt ([TimelineTree `first_input`](../../../crates/dim-agent-capabilities/src/timeline_tree.rs)). Goal there has no goal of its own and writes the start `clear` as on any child ([Goal §3.4](Goal.md#34-a-new-branch-clears-the-inherited-goal)), so it never continues.

## 4. Decisions in Goal

Every decision is taken as Goal takes its own: by the Dispatcher handler from consumed state, through an owned task that rechecks before writing ([Goal A.4](Goal.md#a4-continuation-decision)).

### 4.1 Working branch

| Consumed event | Condition | Action |
| --- | --- | --- |
| `turn/end` N | Goal's C1–C4 hold (active goal, not interrupted, turn `completed`, latest and undecided), the judge is `evaluator`, and the turn contains a worker `completed` block | Instead of Goal's continuation decision: `treeCommands.create(B, N, {prompt, source: {kind: "host", operation_id: "goal:evaluate:N"}}, cancelled)`. Goal's `decided` counts that `branch/create` as `N`'s decision (§3), so a reload does not create again |
| `branch/create-failed` for `goal:evaluate:N`, or `create` throws before any `branch/create` for `N` committed | `N` is still current | Pause (`evaluation_failed`), marked `after="N"` as Goal's pauses are; the first of the two to write it supersedes `N` for the other. A creation that committed records its own outcome (a child or `branch/create-failed`), so that fact decides it, and a throw because the task was cancelled writes nothing |
| Conclusion input for evaluation `N` | The first conclusion for `N`, from the branch named by the `branch/create` for `goal:evaluate:N`, and `N` is still current: no keyed goal block (`set`, `edit`, `clear`, `pause`, `resume`, Goal's own pauses, or a new worker `completed`) follows `N` | `done`: `completed` by the evaluator. `blocked`: `blocked` by the evaluator. `continue`: Goal's continuation decision ([Goal A.4](Goal.md#a4-continuation-decision) C1, C5 and C6), then a continuation with the Pursuit text, or pause (`limit`); as at any turn end, a pending driving input means no continuation (C5). `failed`: pause (`evaluation_failed`) |

Later conclusions for the same `N`, conclusions from another branch, and conclusions for a superseded `N` decide nothing. Whether `N` is current is Goal's own fold by sequence: any later goal change on the working branch is a newer decision, so a late `DONE` never overrides it. Everything else is Goal's: while the evaluator works, the working branch behaves as any active goal (user input, interrupts, pauses and continuations follow Goal's rules).

### 4.2 Evaluator branch

Goal recognizes the role from the branch's first local Queue input (what Prompt appends while `branch/start` dispatches is Append and does not count, as in TimelineTree's creation rule): user text whose first line is `<dim-goal kind="evaluate" branch="B" evaluation="N">`, where `B` is the `parent` of its `branch/start`. With #427, recognition reads the line's source instead ([§13](#13-ending-the-evaluator-with-427)).

| Consumed event | Condition | Action |
| --- | --- | --- |
| `turn/end` M, status `completed` | The reply has a conclusion | Write it to `B` with `treeCommands.input` (Append), marked `evaluation="N" evaluator="E"` |
| `turn/end` M, status `completed` | No conclusion, and `M` is the evaluator's first turn end | Write a follow-up (Queue, `after="M"`) on the evaluator branch asking it to end with one of the three conclusions |
| `turn/end` M, status `completed` | No conclusion, and `M` is not its first turn end (it was already asked once) | Write `continue` to `B` with the whole reply, truncated to 4000 characters, as the feedback |
| `turn/end` M, status `failed` or `cancelled` | — | Write `failed` to `B` |

The reply is the final model request of the turn: the `delta` of every `step/text` with that `turn_id` and the `request_id` of the turn's last `step/model-start`, concatenated in sequence order (a line can span records; earlier requests and thinking are excluded). The conclusion starts at the last line beginning `DONE:`, `CONTINUE:` or `BLOCKED:` and runs to the end, so a `CONTINUE` list can span lines; it is truncated to 4000 characters, as a whole reply written as `continue` is.

Whether the evaluator was already asked is its own history: the turn ending at `M` is its first or a later one, so a reload needs nothing extra. A restored or reloaded part rebuilds from committed history and decides only on the live events it consumes after its cutoff ([branch TypeScript binding](../branch-typescript.md#subscription-rebuild-and-recovery), item 2): a `turn/end` already in history when the evaluator's part loads produces no report (for example the `failed` end that Loop records, while the branch reloads, for a turn the restart cut), and the working branch does not decide again a conclusion it folded at load. The window in which the process stops after the evaluator's `turn/end` commits and before its report is written (or after a conclusion commits and before its outcome is written) is deliberately not handled: no outcome reaches the working branch and its goal stays active with no evaluation running. The user can pause and resume the goal, and the worker's next completion report starts a new evaluation.

## 5. Context mapping

Markers, escaping and keys are Goal's ([A.5](Goal.md#a5-model-visible-texts)). `N` is the request's `turn/end`, `B` the working branch, `E` the evaluator.

| Write | Where, key | Mode | Text |
| --- | --- | --- | --- |
| evaluator prompt | E, first input | Queue | `<dim-goal kind="evaluate" branch="B" evaluation="N">` / `You are an evaluator. Branch B reported that its goal is completed. Only you decide whether it is. Do not continue the work, do not change files or settings, and do not call goal_update.` / blank / `Objective:` / `<objective>` / blank / `Reported result:` / `<summary>` / blank / `Verify the facts yourself by reading files and running checks; do not rely on the report or on earlier messages. End your reply with exactly one of:` / `DONE: <the verified result>` / `CONTINUE: followed by the items that are at least still missing, one per line, each with its evidence` / `BLOCKED: <what only the user can provide>` |
| follow-up | E, no key | Queue | `<dim-goal kind="evaluate-again" after="M" branch="E">` / `Your reply did not end with a conclusion. End your reply with exactly one of DONE:, CONTINUE: or BLOCKED: as described above.` |
| conclusion | B, no key | Append | `<dim-goal kind="report" result="done\|continue\|blocked\|failed" evaluation="N" evaluator="E" branch="B">` / `Evaluator report:` / `<conclusion, or "the evaluator's turn did not complete">` |
| `done` | B, `goal:status` | Append | `<dim-goal kind="goal" op="completed" by="evaluator" evaluation="N" branch="B">` / `Result:` / `<result>` / blank / `An evaluator verified that the goal for branch B is completed. Do not continue working toward it on your own.` |
| `blocked` | B, `goal:status` | Append | `op="blocked" by="evaluator"`; `Blocker:` / `<what is needed>` / blank / `An evaluator found that the goal for branch B cannot proceed without the user. Do not continue working toward it on your own.` |
| `continue` | B, Goal continuation | Queue | `<dim-goal kind="continuation" count="k" limit="L" after="N" evaluation="N" branch="B">` / `An evaluator checked your report that the goal for branch B is completed and found it is not achieved yet. Its report above lists items that are at least still missing; the list may be incomplete. Assess yourself how far the goal still is, then continue working toward it. When you think it is achieved, call goal_update with status "completed" again.` |
| `failed` | B, `goal:status` | Append | Goal's pause with reason phrase "the evaluator could not check the result" |

Under the evaluator judge a worker `completed` block is a request and leaves the fold's status unchanged (`active` while the goal runs); it stays the latest status block until an outcome or a change replaces it. That is the one exception to Goal's fold rule in Goal A16: the replayed status block reads `completed` while the goal is active. The conclusion input carries no key and no `user` content, like Goal's own writes, so it changes no status, is not operator input and resets no count (Goal A5). It is one block, the report: Goal reads a write's marker from the first block ([Goal A.5](Goal.md#a5-model-visible-texts)), so a report anywhere but the first block of its input decides nothing.

## 6. Rule check

Rules: [principles §1 and §4](../../architecture/rust-agent/principles.md), [ADR 002](../../adr/002-plugin-scope.md) D1–D5.

| Rule | How this document complies |
| --- | --- |
| §4.1 Command events for others' state | Creating the evaluator is a command to TimelineTree; the conclusion is an input to the working branch, where Goal, the owner, decides |
| §4.2 No recall after handoff | Nothing is withdrawn; a duplicate conclusion simply decides nothing |
| §4.3 Only before callbacks, never block | Not applicable: no before callback |
| §4.4 No business meaning in core | No change to Timeline, Dispatcher, TimelineTree, InputQueue or Loop |
| §4.5 Batch | Decisions follow consumed events in sequence order; each outcome is one input |
| §4.6 Context only from handed-off events | The worker sees the conclusion only through adopted inputs; the evaluator's reply stays on its branch |
| §4.7 Public events and services only | Goal reads only its own branch's public events and calls exported services; no branch reads another |
| §4.8 Requirements to mechanism owners | None |
| §4.9 Only facts later decisions use enter the Timeline | No new event type; `branch/create` records the evaluation, the conclusion input is what the owner decides on, the follow-up is what makes the evaluator conclude |
| §1 Capability and App Server boundary | No new Capability; nothing in App Server |
| ADR 002 D1 Scope | The evaluation and its outcome are working-branch state of Goal's branch part there; the evaluator role is Goal's branch part on the evaluator |
| ADR 002 D2 No wiring modules | Nothing names Goal |
| ADR 002 D3 Scope dependencies | Unchanged from Goal |
| ADR 002 D4 Configuration | The judge is a per-goal Timeline fact; no new configuration |
| ADR 002 D5 Language-neutral contracts | The `judge` parameter, markers and operation id are data |

## 7. Requirements raised to mechanism owners

None.

## 8. Security

The evaluator is an ordinary child branch with the working branch's permission policy and Approval ([ADR 003](../../adr/003-approval-scope.md)); "do not change anything" is guidance, not isolation. A conclusion counts only when it comes from the branch the working branch's own `branch/create` names, so a host branch created by an operator with a forged prompt decides nothing. Diagnostics record sequences and the result kind, never objective or report text.

## 9. Acceptance

Flows with a fake model for worker and evaluator, in `goal.test.mjs` and `transport_flow/goal.rs`:

1. Evaluator judge: continuations are Goal's. The worker reports `completed`; the status stays `active`; at that turn's end one evaluator is created at `N` with `goal:evaluate:N` and no continuation is written. `DONE` gives `completed` by the evaluator.
2. `CONTINUE` with a two-line list gives the conclusion and one continuation (`count` + 1 over the continuations after `max(A, last_operator)`; neither the worker's request nor the conclusion moves `A` or resets the count, and C5 applies as at any turn end); `BLOCKED` gives `blocked`; at `maxContinuations`, `CONTINUE` gives pause (`limit`).
3. A worker `blocked`, a limit pause, a user interrupt and a user pause create no evaluator.
4. An evaluator reply without a conclusion gets one follow-up on the evaluator branch, and its next conclusion is applied; if the second reply has none either, the whole reply, truncated to 4000 characters, is written as `continue` (one continuation under `maxContinuations`), and no further follow-up is written, also after a reload.
5. A failed or cancelled evaluator turn, a `branch/create-failed`, and a `create` that throws before any `branch/create`, give one pause (`evaluation_failed`).
6. Restart during the evaluation: no second evaluator; a `turn/end` already in history when the restored evaluator loads, such as the `failed` end of a turn the restart cut, produces no report.
7. A conclusion from a branch that is not the recorded evaluator decides nothing.
8. The streamed conclusion is reassembled across `step/text` records and ignores an earlier request's `DONE:`.
9. Model judge: Goal's acceptance unchanged.
10. A conclusion that arrives after `set`, `edit`, `clear`, `pause`, `resume` or a new worker `completed` on the working branch decides nothing: a late `DONE` after a `pause` leaves the goal paused.

## 10. Later options

Not needed for this design; possible additions if users ask:

| Option | Note |
| --- | --- |
| Read-only or cheaper evaluator | Needs a branch configured before its first input and a per-branch Permission narrowing; neither exists |
| CLI `/pursue` | `/goal` sets a goal whose completion the model reports; `/pursue` would be `/goal` with `judge: "evaluator"`, which no client form sends yet ([CLI commands § Goal](../cli-commands.md#goal)) |

## 11. Ablation review

What is left, and why each piece is needed:

| Piece | Why |
| --- | --- |
| `judge` on `set` | Which goals are judged by an evaluator |
| Worker `completed` as a request under the evaluator judge | Only the evaluator completes the goal (the exception in Goal A16) |
| Create at the stopping `turn/end` | The trigger; `branch/create` is also what keeps a reload from creating twice |
| Evaluator role in Goal's branch part | The evaluator branch must report its conclusion, and only a part on that branch can read its reply |
| Conclusion input to the working branch | The only cross-branch path; Goal there decides |
| First conclusion from the recorded evaluator, while `N` is current | An operator could create a lookalike branch, and a late conclusion must not override a newer goal change |
| One follow-up when the reply has no conclusion, then `continue` | Otherwise the evaluation would end without a result, or ask forever |
| `failed` on a failed or cancelled evaluator turn or creation | These are terminal facts; without it the goal would stay active with no evaluation running |

Removed from the earlier draft: claim and reopen states, deadline and `evaluationTimeoutSeconds`, the `goal/evaluation-reported` record, special rules for user input during an evaluation, and the requirements to stop and retire branches. That stays true for this design, which raises no requirement. Ending a line is now a generic operation of [Timeline branches](../timeline-branches.md#10-ending-a-line), and §13 uses it.

## 12. Ready confirmation

No product decision remained. The maintainer confirmed the design as Ready on 2026-10-26 ([#146](https://github.com/arcships/dim/issues/146)), including the A16 fold exception in [Goal](Goal.md#appendix-e-assumptions) (decided 2026-10-22). It is implemented ([#422](https://github.com/arcships/dim/issues/422)), and [Goal](Goal.md) describes both judges.

## 13. Ending the evaluator (with #427)

Decided with [Timeline branches](../timeline-branches.md) ([#427](https://github.com/arcships/dim/issues/427)), and implemented together with its line end. Until then, §4.2 holds as written. All changes are in Goal's parts.

| # | Change |
| --- | --- |
| E1 | **Recognition from the line's source.** Goal on a line takes the evaluator role when `treeCommands.info(own id)` has `source = {kind: "host", operation_id: "goal:evaluate:N"}` and `parent = B`. It no longer reads the prompt's first line. The prompt keeps its `<dim-goal kind="evaluate" …>` first line, for the model only (§5) |
| E2 | **Goal ends its evaluator.** Evaluation `N` is settled on `B` once its conclusion or failure is decided (§4.1), or once a later goal change supersedes `N`. When it is settled, Goal on `B` ends `E` with `treeCommands.end(E, {kind: "host", operation_id: "goal:evaluate:N"})`, through an owned task that rechecks first. If `B`'s history already has a `branch/ended` for `E`, Goal does nothing. At load the same check reconciles: a recorded evaluator whose `N` is settled and that has no `branch/ended` on `B` is ended |
| E3 | **An outside end before the conclusion is `failed`.** A `branch/ended` for `E` on `B` that comes before `E`'s conclusion, while `N` is current, is decided as `failed`: pause (`evaluation_failed`), as in §4.1 |

Interfaces:

- Goal's branch part adds the Dispatcher key `branch/ended`.
- Goal's branch part injects `sessionLifecycle` (`activated()`): an end at load waits for the Session to publish the lines its restore opens (E2).
- The branch TypeScript binding exposes `end(id, source)` and `TimelineInfo.source` in the places that expose `create` and `input` today:
  - the generated `npm/branch-services.d.ts`;
  - the `treeCommands` row of the [binding table](../branch-typescript.md);
  - Goal's own `TreeCommands` type in `branch.ts`.

Acceptance, with a fake model, in `goal.test.mjs` and `transport_flow/goal.rs`:

1. The evaluator is recognized from its source. A host line whose prompt has the same first line but whose operation id is different takes no role.
2. After `DONE`, `CONTINUE`, `BLOCKED` or `failed` is decided, `E` is ended exactly once, with source `goal:evaluate:N`, and `B` records `branch/ended` for it.
3. A superseded `N`, for example after a `pause` on `B`, ends `E`.
4. Restart after a settled `N` that has no `branch/ended` ends `E` once at load. When `branch/ended` is already there, nothing is written.
5. A user end of `E` before its conclusion pauses the goal (`evaluation_failed`). A user end after the conclusion decides nothing.
