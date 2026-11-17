# Wait: deriving the environment from its callers

Scope: evidence for the environment assumptions of the [Wait experiment](wait-contract-experiment.md).
Issue: [#1615](https://github.com/arcships/dim/issues/1615).

This review follows the local-execution profile at `8e69a812a`. It is not a new runtime contract,
an inventory of all possible plugins, or a production-traffic study. Source describes current
behavior, owner documents supply contracts, and flow tests exercise selected complete paths.
No user's stored sessions were inspected.

## 1. A concrete use, before the abstract machine

The model starts a background command and receives its task id. While its turn remains open,
it calls the `wait` tool, optionally naming that task. ToolExecutor runs that call with the turn's
cancellation token. Wait resolves the caller's branch service and awaits an eligible arrival,
timeout or caller cancellation. Its result returns through ToolExecutor as a tool result.

The background task's result reaches Wait indirectly:

```text
command completes
  -> exec/ended
  -> producer writes tagged input/message (steer mode)
  -> Dispatcher delivers it
       -> Wait records an arrival if a turn is open
       -> InputQueue independently applies the input for Loop/context
```

When idle, the input can drive a new turn through InputQueue and Loop. Wait ignores that
pre-turn arrival; this does not discard the Timeline input or prevent the model seeing it.
Thus "every completion must eventually be returned by a wait call" would be the wrong goal.
Wait is neither a durable inbox nor a join operation for the external process.

## 2. Where the assumptions come from

| Environment question | Evidence | Consequence for the argument |
| --- | --- | --- |
| Which profile actually installs Wait? | [defaults.rs](../../../crates/dim-agent-capabilities/src/defaults.rs), `install_local_execution`; [workspace_tools/mod.rs](../../../crates/dim-agent-capabilities/src/workspace_tools/mod.rs), `install` | Local execution installs Wait and BackgroundExec. Do not infer a global assembly from a shared event type |
| Who calls the tool? | [wait.rs](../../../crates/dim-agent-capabilities/src/wait.rs), `WaitPlugin`, `WaitTool::run`; [tool_executor.rs](../../../crates/dim-agent-capabilities/src/tool_executor.rs), `execute` | The selected tool handler receives a branch/turn context and caller cancellation; the task id comes from the model's call, not a Wait-owned task table |
| Is the tool the only entry? | Wait's public service and the [owner's data face](../../architecture/rust-agent/plugins/Wait.md#data-face) | Other plugins can call the service. Tool timeout limits cannot be assumed for every service caller; bridge callers must signal cancellation before abandoning the reply |
| What produces wake facts? | [background.rs](../../../crates/dim-agent-capabilities/src/workspace_tools/background.rs), `ended`, `wake`, `wake_input`, `wakes` | A successful notification append follows the terminal fact. Completed/failed tasks can wake; cancelled tasks do not. Branch closing suppresses new wakes. Posting can fail, so a terminal is not by itself a delivered notification |
| Are these producers exclusive? | [shell_session.rs](../../../crates/dim-agent-capabilities/src/shell_session.rs), `settle`, also uses `wake_input`; `install_minimal_execution` in defaults | There is another producer of the same format in a different profile. That installer does not itself install Wait. Shared format is evidence of compatibility, not proof that the two profiles co-install all consumers |
| What happens when there is no open turn? | [input_queue.rs](../../../crates/dim-agent-capabilities/src/input_queue.rs), `input`, `release`; [agent_loop.rs](../../../crates/dim-agent-capabilities/src/agent_loop.rs), `driven`, `try_start` | A steer can become applied input and start a new turn. Its absence from Wait's pending state is intentional |
| Can turns occur in arbitrary order? | AgentLoop's `try_start` and `consume` handling of `TurnStart`/`TurnEnd` | The real Loop checks turn identity and phase. Arbitrary repeated starts/ends in the isolated experiment are a broader input space, not reachable product traces |
| Who cancels an active wait? | ToolExecutor's `StopInterrupt` handling and generation token; Wait's `arrival`; [host.rs](../../../crates/dim-agent-capabilities/src/host.rs), `call_until_cancelled` | Wait does not subscribe to stop events; the caller propagates cancellation. Cancelling this wait does not imply cancelling the background command |
| Does reload reconnect an existing call? | WaitTool resolves an `Arc<Wait>` per call; WaitBranch installs a fresh service and subscription; owner contract says futures are caller-owned | The current recovery model omits active old calls. The lifecycle contract requires settlement; caller ownership of the future does not exempt the service generation from ending its work |

## 3. Check the whole path, not just the local transitions

Existing [local_execution.rs](../../../tests/integration/tests/local_execution.rs) flows supply
specific integration evidence:

- `wait_ends_on_the_matching_completion_not_on_the_input_that_started_the_turn`: actual background
  execution and model tool calls; a matching task wakes the wait, a nonmatching task does not.
- `background_completion_is_recorded_once_and_wakes_with_a_bounded_notice`: a terminal precedes its
  correlated steer notification, and the notification drives a later turn.
- `a_turn_stop_keeps_background_work_and_its_wake_starts_a_turn`: stopping the turn leaves detached
  work running; its later result starts a new turn, and a subsequent wait does not consume it again.
- `closing_a_branch_cancels_only_its_tasks_without_a_wake`: closing one branch cancels its tasks
  without manufacturing a wake, while the other branch's work remains independent.

These use a scripted model and real execution components with isolated directories; they are
integration experiments, not observations of every production schedule. Reuse their evidence
rather than making Wait re-prove BackgroundExec or Loop internals. Run outcomes belong in the PR.

## 4. Corrections to model scope

The [notification experiment](wait-contract-experiment.md) is a component-boundary check with an
idle LoopState. Keep its broad traces as robustness evidence, but do not use their reachability
to invent requirements for the product. Its successful tagged inputs assume the producer has
already posted the notification; they make no claim that every terminal will post successfully.
Its zero-timeout reads exercise the public service, not the model-facing tool's timeout validation.

The [TLC recovery model](wait-argument.md) omits active calls. Its successful new-generation
handoff therefore cannot establish compliance with the [capability reload protocol](../../architecture/rust-agent/single-tree.md#能力重载协议).
That protocol already requires unfinished operations to end with `capability reloading`; tool
results carry the reload-settlement marker, and Loop ends the turn as failed instead of advancing.
[Time bus §5](../../architecture/rust-agent/time-bus.md#5-副作用不重放) makes capability reload and
process restart equivalent for the new generation. This is an existing requirement, not a choice
between retaining, migrating or cancelling old waits.

The implementation inspected before the [reload fix in #1619](https://github.com/arcships/dim/pull/1619)
holds an `Arc<Wait>` in each call but does not connect its wait to
WaitBranch's generation cancellation. Losing the subscription does not finish that future.
A full caller/ToolExecutor experiment confirmed that an old call can survive service replacement
until a later turn stop, while the new service receives new notifications. This is a lifecycle
compliance defect, not merely unclear wording about single-call exclusivity. The experimental
assertions requiring that survival are not retained as regression expectations; results and the
reproduction commit are recorded in the PR.

The fix must connect generation termination to the accepted call's settlement and propagate the
reload reason through the existing tool-result path. Wait must not write ToolExecutor's terminal
facts itself. Ordinary caller cancellation remains a separate input. Recovery restores durable
business state, not old waiting futures. A corrected regression must show that WaitBranch-only
reload ends the old tool call without an extra user stop, marks reload settlement and prevents
Loop from advancing that turn. The general case also needs the public service and retained-handle
admission boundary considered; this documentation PR does not claim a runtime fix.

New plugin integrations should supply the same boundary information: admission, event meaning,
ordering, call cancellation and reload responsibility. They need not appear on a fixed list of
writers/consumers. An assumption without a contract owner or integration evidence stays explicit;
it cannot be justified by the isolated model's success.
