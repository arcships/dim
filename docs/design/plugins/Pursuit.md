# Pursuit

Status: Draft for the Goal termination/reload contract in [#1320](https://github.com/arcships/dim/issues/1320). Scope: the evaluator judge of the existing [Goal](Goal.md) Session and branch parts; no new Capability, service or configuration. [Goal state](Goal-state-read.md) owns execution lifetime and reload behavior; this document owns evaluator decisions and model-facing reports.

Requirement ([feature list](../../feature-list.md), Agent behavior): keep working toward an explicit objective and record its completion or blockage, **with completion judged by an evaluator instead of the model doing the work**.

**Pursuit is a judge mode of Goal.** A goal set with `judge: "evaluator"` runs exactly as any Goal goal, with Goal's continuations, interrupt, pause and explicit limit. One thing changes: when a turn of the working branch ends and Goal does not continue because the worker reported the goal completed, Goal creates an **evaluator branch**. The evaluator checks the work in one turn and ends it with a conclusion: `DONE`, `CONTINUE` with what is at least still missing and the evidence, or `BLOCKED` with what only the user can provide. When the evaluator's turn ends, Goal on the evaluator writes the conclusion back to the working branch as an input, and Goal on the working branch decides: `DONE` completes the goal (only the evaluator can), `CONTINUE` becomes one continuation, `BLOCKED` blocks it. This replaces the experimental design (#17).

## 1. Decisions

| # | Decision | Why |
| --- | --- | --- |
| P1 | A judge mode of Goal, not a separate plugin | One owner of continuation; a separate plugin would race Goal on the same `turn/end` or read Goal's state (§4 rule 7) |
| P2 | The judge is chosen per goal by `set`, recorded on the `set` block | It is the user's intent for that goal, a Timeline fact |
| P3 | Evaluate when the worker's completion report stops Goal | That is the only point where the worker decides the goal is done. A worker's `blocked` stands (it needs the user); a limit, or the user's own interrupt or pause, is not a completion and starts nothing |
| P4 | Under the evaluator judge the worker's `completed` is a request; the status stays `active` | Only the evaluator completes the goal. The current execution records a completion request without changing the business status; see [Goal A16](Goal.md#appendix-e-assumptions) |
| P5 | The evaluator is a child branch cut at that `turn/end` | It sees what was done and why, runs under the same policy, and is an ordinary branch created with `treeCommands.create` |
| P6 | The evaluator writes its conclusion to the working branch as input; Goal there decides | A branch folds only its own events; cross-branch communication is input by id ([plugin contract §5.2](../plugin-contract.md#52-consuming-facts)), and a line reads the line it was cut from through a tool result ([Timeline branches](../timeline-branches.md) §6). Callers write, owners decide |

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

Clients read current Goal status through [session.goal.get](Goal.md#a3-reading-the-goal). Working-branch history still shows the judge, evaluator creation, report and outcome, but a historical active record does not prove that the current instance is running. After reload an unfinished goal is paused in memory; its old evaluator cannot reactivate it. A worker completed report remains an evaluation request, not a completed status.

## 3. Changes to Goal

| Part | Change |
| --- | --- |
| `session.rs` | `session.goal.set` accepts `judge?: "model" \| "evaluator"` (default `model`; other values `invalid_params`) and marks the `set` objective block `judge="evaluator"` |
| `fold.rs` | The current execution keeps judge, completion requests, evaluator identities, first reports and decision deduplication in memory. Worker completed does not change active status or reset counters. No evaluator map, pending conclusion or counter is persisted for execution recovery. |
| `branch.rs` | Adds the Dispatcher keys `branch/create` and `branch/create-failed`; creates the evaluator; on an evaluator branch, takes the evaluator role (§4.2) |

No new service, Settings field or event type, and no new injection beyond Goal's `sessionLifecycle` (§13). The evaluator branch is an ordinary host-sourced child; its first Queue input is the prompt ([TimelineTree `first_input`](../../../crates/dim-agent-capabilities/src/timeline_tree.rs)). Goal there has no goal of its own and writes the start `clear` as on any child ([Goal §3.4](Goal.md#34-a-new-branch-clears-the-inherited-goal)), so it never continues.

## 4. Decisions in Goal

Every decision is taken as Goal takes its own: by the Dispatcher handler from consumed state, through an owned task that rechecks before writing ([Goal A.4](Goal.md#a4-continuation-decision)).

### 4.1 Working branch

| Consumed event | Condition | Action |
| --- | --- | --- |
| `turn/end` N | Goal's C1–C4 hold (active goal, not interrupted, turn `completed`, latest and undecided), the judge is `evaluator`, and the turn contains a worker `completed` block | Instead of Goal's continuation decision: `treeCommands.create(B, N, {prompt, source: {kind: "host", operation_id: "goal:evaluate:N"}}, cancelled)`. Goal's `decided` counts that `branch/create` as `N`'s decision (§3), within this execution; reload pauses the goal and never creates the old evaluator again |
| `branch/create-failed` for `goal:evaluate:N`, or `create` throws before any `branch/create` for `N` committed | `N` is still current | Pause (`evaluation_failed`), marked `after="N"` as Goal's pauses are; the first of the two to write it supersedes `N` for the other. A creation that committed records its own outcome (a child or `branch/create-failed`), so that fact decides it, and a throw because the task was cancelled writes nothing |
| Conclusion input for evaluation `N` | The first conclusion for `N`, from the branch named by the `branch/create` for `goal:evaluate:N`, and `N` is still current: no newer change follows `N` — a `set`, `edit`, `clear`, `pause` or `resume` trigger, a new worker `completed`, or an old keyed block. A model-facing `goal:state` input is not another completion request. The current business state and in-memory evaluation identity determine whether the conclusion is still eligible; paused stops it | `done`: `completed` by the evaluator. `blocked`: `blocked` by the evaluator. `continue`: Goal's continuation decision ([Goal A.4](Goal.md#a4-continuation-decision) C1, C5 and C6, C6 only when `maxContinuations` is set, and the background wait), then a continuation with the Pursuit text, or pause (`limit`); as at any turn end, a pending driving input means no continuation (C5), and a background task still open means no decision yet: the current execution keeps the conclusion in memory, and the cancelled end that closes the last open task may take it while that execution remains running. `failed`: pause (`evaluation_failed`) |

Later conclusions for the same `N`, conclusions from another branch, and conclusions for a superseded `N` decide nothing. Whether `N` is current is determined by the current execution's accepted facts in sequence: any later goal change on the working branch is a newer decision, so a late `DONE` never overrides it. Everything else is Goal's: while the evaluator works, the working branch behaves as any active goal (user input, interrupts, pauses and continuations follow Goal's rules).

### 4.2 Evaluator branch

The evaluator identity comes from the line source and parent ([§13](#13-ending-the-evaluator-with-427)), not from parsing historical prompt text. Execution starts only when this instance consumes the new host-created opening Queue input after its load cutoff. TimelineTree finishes opening the parts before appending that first input. A reloaded instance does not replay the old opening input, so source metadata alone cannot restart evaluator work. The one-follow-up flag and report task belong to this execution only.

| Consumed event | Condition | Action |
| --- | --- | --- |
| `turn/end` M, status `completed` | The reply has a conclusion | Write it to `B` with `treeCommands.input` (Append), marked `evaluation="N" evaluator="E"` |
| `turn/end` M, status `completed` | No conclusion, and `M` is the evaluator's first turn end | Write a follow-up (Queue, `after="M"`) on the evaluator branch asking it to end with one of the three conclusions |
| `turn/end` M, status `completed` | No conclusion, and `M` is not its first turn end (it was already asked once) | Write `continue` to `B` with the whole reply, truncated to 4000 characters, as the feedback |
| `turn/end` M, status `failed` or `cancelled` | — | Write `failed` to `B` |

The reply is the final model request of the turn: the `delta` of every `step/text` with that `turn_id` and the `request_id` of the turn's last `step/model-start`, concatenated in sequence order (a line can span records; earlier requests and thinking are excluded). The conclusion starts at the last line beginning `DONE:`, `CONTINUE:` or `BLOCKED:` and runs to the end, so a `CONTINUE` list can span lines; it is truncated to 4000 characters, as a whole reply written as `continue` is.

The evaluator's one-follow-up flag is current-execution memory. Reload does not read old model replies, reconstruct the flag, send a missing report or retry a conclusion. On controllable termination the owning Goal cancels and settles its work and writes paused on the working branch. After a crash, loading unfinished Goal state produces paused in memory without writing another state event.

The working branch only accepts reports paired with an evaluator created by its current execution. A new instance starts with an empty pairing map; a report from an old evaluator is ignored even after the user resumes the same objective. Only a new set/resume permits new work and a new evaluation. Resource cleanup uses the original lifecycle owners; it does not reconstruct Goal evaluation progress.

## 5. Context mapping

Markers, escaping and keys are Goal's ([A.5](Goal.md#a5-model-visible-texts)). `N` is the request's `turn/end`, `B` the working branch, `E` the evaluator.

| Write | Where, key | Mode | Text |
| --- | --- | --- | --- |
| evaluator prompt | E, first input | Queue | `<dim-goal kind="evaluate" branch="B" evaluation="N">` / `You are an evaluator. Branch B reported that its goal is completed. Only you decide whether it is. Do not continue the work, do not change files or settings, and do not call goal_update.` / blank / `Objective:` / `<objective>` / blank / `Reported result:` / `<summary>` / blank / `Verify the facts yourself by reading files and running checks; do not rely on the report or on earlier messages. End your reply with exactly one of:` / `DONE: <the verified result>` / `CONTINUE: followed by the items that are at least still missing, one per line, each with its evidence` / `BLOCKED: <what only the user can provide>` |
| follow-up | E, no key | Queue | `<dim-goal kind="evaluate-again" after="M" branch="E">` / `Your reply did not end with a conclusion. End your reply with exactly one of DONE:, CONTINUE: or BLOCKED: as described above.` |
| conclusion | B, no key | Append | `<dim-goal kind="report" result="done\|continue\|blocked\|failed" evaluation="N" evaluator="E" branch="B">` / `Evaluator report:` / `<conclusion, or "the evaluator's turn did not complete">` |
| `done` | B, `goal:state` (Goal's record) | Append | `status="completed" judge="evaluator" after="<conclusion>"`; body: `Objective:` / `<objective>` / blank / the objective policy / blank / `Result:` / `<result>` / blank / `An evaluator verified that the goal for branch B is completed. Do not continue working toward it on your own.` |
| `blocked` | B, `goal:state` (Goal's record) | Append | `status="blocked" judge="evaluator" after="<conclusion>"`; body: `Objective:` / `<objective>` / blank / the objective policy / blank / `Blocker:` / `<what is needed>` / blank / `An evaluator found that the goal for branch B cannot proceed without the user. Do not continue working toward it on your own.` |
| `continue` | B, Goal continuation | Queue | `<dim-goal kind="continuation" count="k" limit="L" after="N" evaluation="N" idle="k" branch="B">` / `An evaluator checked your report that the goal for branch B is completed and found it is not achieved yet. Its report above lists items that are at least still missing; the list may be incomplete. Assess yourself how far the goal still is, then continue working toward it. When you think it is achieved, call goal_update with status "completed" again.`; `limit` is written only when Goal's document sets one; `idle` is `0`: the evaluated path does not advance Goal's C7 streak |
| `failed` | B, `goal:state` (Goal's record) | Append | `status="paused" reason="evaluation_failed" after="<conclusion>"`; Goal's pause with the reason phrase "the evaluator could not check the result" |

The evaluator's outcome is a **trigger**, and Goal writes the state: the conclusion input carries no key and no `user` content, so it is not a state statement, not operator input and resets no count (Goal A5); Goal answers it with one `goal:state` record whose `after` names the conclusion. Under the evaluator judge a worker `completed` trigger is a request: it leaves the state unchanged, and the record Goal answers it with still states `active` (Goal A16), so the model and the clients need no exception of their own. The conclusion is one block, the report: Goal reads a write's marker from the first block ([Goal A.5](Goal.md#a5-model-visible-texts)), so a report anywhere but the first block of its input decides nothing.

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
| §4.7 Public events and services only | Goal reads only its own branch's public events and calls exported services; no part folds another branch's facts. The evaluator's reading of the worker's work is a tool result on its own branch, not a fold ([Timeline branches](../timeline-branches.md) §6) |
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

Flows with a fake model for worker and evaluator, in the crate's flow tests under `crates/dim-goal/tests`:

1. Evaluator judge: continuations are Goal's. The worker reports `completed`; the status stays `active`; at that turn's end one evaluator is created at `N` with `goal:evaluate:N` and no continuation is written. `DONE` gives `completed` by the evaluator.
2. `CONTINUE` with a two-line list gives the conclusion and one continuation (`count` + 1 over the continuations after `max(A, last_operator)`; neither the worker's request nor the conclusion moves `A` or resets the count, and C5 applies as at any turn end); `BLOCKED` gives `blocked`; at an explicit `maxContinuations`, `CONTINUE` gives pause (`limit`); the evaluated continuation does not read progress or advance Goal's C7 streak, because the evaluator judged the worker's turn with the whole of it in front of it. A background task still open when the conclusion arrives writes no continuation; the conclusion stays in current-execution memory, and the cancelled end that closes the last open task takes it once while the execution is running (Goal A.4).
3. A worker `blocked`, a limit pause, a user interrupt and a user pause create no evaluator.
4. An evaluator reply without a conclusion gets one follow-up on the evaluator branch, and its next conclusion is applied; if the second reply has none either, the whole reply, truncated to 4000 characters, is written as `continue` (one continuation under `maxContinuations`), and no further follow-up is written, within the same execution. Reload terminates the evaluation rather than reconstructing its follow-up state.
5. A failed or cancelled evaluator turn, a `branch/create-failed`, and a `create` that throws before any `branch/create`, give one pause (`evaluation_failed`).
6. Restart during evaluation: the working goal is paused in memory, no second evaluator or missing report is produced, and late old reports cannot complete or continue the goal. Explicit resume starts a new execution with empty counters and evaluation identities.
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
| Create at the stopping `turn/end` | The current execution pairs one evaluator to its decision. Reload pauses instead of recreating that operation. |
| Evaluator role in Goal's branch part | The evaluator branch must report its conclusion, and only a part on that branch can read its reply |
| Conclusion input to the working branch | The only cross-branch path; Goal there decides |
| First conclusion from the recorded evaluator, while `N` is current | An operator could create a lookalike branch, and a late conclusion must not override a newer goal change |
| One follow-up when the reply has no conclusion, then `continue` | Otherwise the evaluation would end without a result, or ask forever |
| `failed` on a failed or cancelled evaluator turn or creation | These are terminal facts; without it the goal would stay active with no evaluation running |

Removed from the earlier draft: claim and reopen states, deadline and `evaluationTimeoutSeconds`, the `goal/evaluation-reported` record, special rules for user input during an evaluation, and the requirements to stop and retire branches. That stays true for this design, which raises no requirement. Ending a line is now a generic operation of [Timeline branches](../timeline-branches.md#10-ending-a-line), and §13 uses it.

## 12. Ready confirmation

The evaluator judge is part of Goal. Its execution lifetime follows [Goal state](Goal-state-read.md), including memory-only progress, durable terminal states and reload-to-paused. No execution recovery mode or persistent evaluation-state protocol is left for implementation to choose.

## 13. Ending the evaluator (with #427)

Evaluator identity and branch ending use the existing [Timeline branches](../timeline-branches.md) contract. The following rules operate within the current Goal execution; reload behavior is defined in §4.2.

| # | Change |
| --- | --- |
| E1 | **Recognition from the line's source.** Goal on a line takes the evaluator role when `treeCommands.info(own id)` has `source = {kind: "host", operation_id: "goal:evaluate:N"}` and `parent = B`. It no longer reads the prompt's first line. The prompt keeps its `<dim-goal kind="evaluate" …>` first line, for the model only (§5) |
| E2 | **Goal ends its evaluator.** Evaluation `N` is settled on `B` once its conclusion or failure is decided (§4.1), or once a later goal change supersedes `N`. When it is settled, Goal on `B` ends `E` with `treeCommands.end(E, {kind: "host", operation_id: "goal:evaluate:N"})`, through an owned task that rechecks first. If the current execution has consumed the terminal fact for E, Goal does nothing. This is cleanup of work owned by the current execution, not a requirement to load historical evaluator relations or replay old decisions |
| E3 | **An outside end before the conclusion is `failed`.** A `branch/ended` for `E` on `B` that comes before `E`'s conclusion, while `N` is current, is decided as `failed`: pause (`evaluation_failed`), as in §4.1 |

Interfaces:

- Goal's branch part adds the Dispatcher key `branch/ended`.
- Goal's branch part uses sessionLifecycle and branchLifecycle: new work waits for publication, and current owned work is cancelled/settled before closing. Loading does not start historical evaluator work.
- Goal's branch part uses `TreeCommands`'s `create`, `input`, `end` and `info` through their Rust exports, and reads `TimelineInfo.source` to recognize an evaluator line.

Acceptance, with a fake model, in the crate's flow tests under `crates/dim-goal/tests`:

1. The evaluator is recognized from its source. A host line whose prompt has the same first line but whose operation id is different takes no role.
2. After `DONE`, `CONTINUE`, `BLOCKED` or `failed` is decided, `E` is ended exactly once, with source `goal:evaluate:N`, and `B` records `branch/ended` for it.
3. A superseded `N`, for example after a `pause` on `B`, ends `E`.
4. Reload does not load an evaluation map, complete old reports or create another evaluator. Previously owned resource cleanup is checked separately through its lifecycle owner; a late old report cannot change the paused goal.
5. A user end of `E` before its conclusion pauses the goal (`evaluation_failed`). A user end after the conclusion decides nothing.
