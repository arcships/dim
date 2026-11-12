# Pending input read

InputQueue's existing Session part registers the nonmutating `session.input.pending` method.
It takes `sessionId`, `branchId`, optional `through` and optional Timeline `after` cursor.
It returns `branchId`, the fixed published `through`, `inputs` and `next`.
Each page contains at most 128 inputs in ascending branch sequence order. Continuations retain
`through`; a failure is not an empty inventory.

Each input contains `inputId`, `sequence`, effective `mode`, optional `user`, and `applied`.
Candidates are branch-local ordinary `input/message` facts, excluding handover inputs, without matching `input/consumed` or
`input/deleted` at the cutoff. Only identity, sequence, mode and user content are projected;
internal context blocks are not returned. A second query over the selected identities determines
which have `input/applied`. Missing stored mode follows the input format's existing default:
Queue when user content exists, Append otherwise.

This read has no retained event list or state cache. It uses TimelineTree.query, including its fixed head for ended branches, and the same published
cutoff for candidate selection and applied-state lookup. The method owns no task beyond its
request; interruption cancels it. InputQueue's write admission remains authoritative for withdraw.
Clients install the inventory independently of transcript history and consume later facts above
its cutoff. Applied inputs remain pending until consumed; they cannot be treated as withdrawable.

## Turn identity read

Loop owns `session.turn.get`, registered by its Session part `LoopMethods` with declared
TreeCommands and SessionMethods dependencies. It takes `sessionId`, `branchId`, optional
`through`, optional `turnId` or `inputId`; returns `branchId`, published `through`, and nullable `turn`.
`turnId` and `inputId` are mutually exclusive and a supplied identity must be nonempty.
A turn has `turnId`, boundary `sequence`, and `ended`; an ended boundary also carries its
original typed `outcome` (`status` and `reason` or `error`). Without an identity the query selects the
latest turn boundary. With one it selects that turn's latest boundary; this resolves old fork
anchors without transcript replay. `inputId` addresses the turn that actually consumed that
accepted input: find its `input/consumed` by input identity, then the last `turn/start` at or
before that position, then that turn’s latest boundary at the original cutoff. An unconsumed
or unknown input returns null and does not fall back to the currently running turn. These are
three indexed position/identity reads, with no input/output body or historical paging.
The first read selects consumed/deleted by the same input identity; a deleted input returns
`inputDeleted=true`, null turn and null output. Other replies may omit `inputDeleted` (false).
For an explicitly selected failed turn, one further indexed lookup returns optional
`turn.modelOutcome`, the latest `ModelOutputOutcome` recorded for that turn. This preserves
the actual model failure classification when the terminal model event is outside the window;
absence means no such model terminal exists, not an inferred failure reason.
Null means no matching boundary, not an inferred idle state
from a short transcript. Fork requires a matching ended turn. The reply contains no event or output body.
Current uncommitted output remains ModelProvider's public progress, including the original block
identities and offsets; it is not derived by rereading the complete active turn.

LoopMethods is a stateless read part, not a Capability: it decides no behavior and writes no
facts. It is Session scoped because the public method addresses any branch. Its read is cancelled
with method admission and retains no history. It adds no wiring part, service or direct owner
state access. These choices satisfy principles §4 rules 1–9, §1 and ADR 002 D1–D5; configuration,
context injection and I/O effect settlement are inapplicable to this read.

The current-turn reply also has nullable `output`: `requestId`, `textCharacters`,
`thinkingCharacters` for the latest model request without model-output-end at the same cutoff.
Character counts sum Unicode scalar values in its text/thinking deltas. They do not return those
deltas. Clients install these absolute offsets after projecting the truncated window, before
consuming later durable events and progress. An identity lookup (`turnId` or `inputId`) does not read output or calculate character counts.
