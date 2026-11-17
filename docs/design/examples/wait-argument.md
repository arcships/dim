# WaitBranch: a recovery and consumption argument

Scope: WaitBranch's open-turn projection and its fixed-cutoff handoff; not the whole Wait tool.
Issue: [#1615](https://github.com/arcships/dim/issues/1615).

This is an optional worked argument, not a required design format for other Capabilities.
See the [design guide](../capability-design-arguments.md) for the default approach.
The [Wait contract](../../architecture/rust-agent/plugins/Wait.md#waitbranch) remains authoritative.
No event, runtime state, persistence format or recovery behavior is proposed here. The executable
model is [WaitRecovery.tla](wait-model/WaitRecovery.tla), with [finite configuration](wait-model/WaitRecovery.cfg).

## 1. Question and assumptions

Can WaitBranch recover whether a turn is open by selecting only the last turn boundary, then
consume later events without losing or repeating their positions?

| Assumption | Source or abstraction boundary |
| --- | --- |
| Facts have an immutable total sequence within one branch | [Time bus §1](../../architecture/rust-agent/time-bus.md#1-只认序号) |
| A fixed-cutoff query observes that prefix, including the permitted publishing tail; errors fail recovery rather than mean empty history | [Property query §2–3](../timeline-property-query.md) |
| Subscribe holds live delivery while rebuilding; live delivery then resumes strictly above the returned cutoff in order | [Plugin contract §5.2](../plugin-contract.md#52-consuming-facts) |
| `turn/start` sets open; `turn/end` clears it; other facts do not change it | [WaitBranch](../../architecture/rust-agent/plugins/Wait.md#waitbranch) |
| Reload restores open-turn status, not old arrival notifications | Same owner contract; no claim that pre-reload wait outcomes are reproduced |

The model treats append as an eligible immutable fact, abstracting storage admission/publication
under the query guarantee. It represents holding by retaining event positions until handoff,
not by reproducing Dispatcher buffers. Facts of all kinds advance the model cursor; an irrelevant
fact is a stuttering step for Wait's projection even when the real filter does not deliver it.
The proof therefore relies on these mechanism guarantees; it does not verify their implementation.

Pending notifications, active wait calls, deadlines, caller cancellation, external tasks and
resource cleanup are outside this projection. They cannot change its `open` value, so omitting
them preserves the propositions below. They do affect full-tool behavior and need separate
arguments before claiming verification of Wait as a whole. In particular, no completion or
cancellation guarantee about a network call or process follows from this example.

## 2. Definitions and allowed transitions

Let H be a finite branch history and c a position between zero and its length. Let B(H,c) be the
subsequence of `turn/start` and `turn/end` at positions at most c. Define:

```text
OpenAt(H,c) = false                         if B(H,c) is empty
              last(B(H,c)) = turn/start     otherwise
```

State: history H; consumed position p; Boolean open; phase live/holding/querying; chosen cutoff c;
generation base b; and D, a proof-only sequence of positions delivered since the last recovery.
Initial state has empty H and D, p=b=c=0, phase live and open=false.

| Action / guard | State change | Facts or work |
| --- | --- | --- |
| Append | Extend H by one immutable event | Environment input, not a Wait-produced fact |
| Deliver, live and p < length(H) | p := p+1; apply that fact's open/close/no-change transition; append p to D | No external work |
| BeginRecovery, live | phase := holding | Abstract new generation's subscription before reading |
| TakeCut, holding | c := length(H); phase := querying | The fixed query range is now established |
| Recover, querying | open := OpenAt(H,c); p := c; b := c; D := empty; phase := live | No old event handlers or external effects are run |

Append may occur between any two actions, including between TakeCut and Recover. Old D belongs
to the previous generation and is discarded; the model does not claim exactly-once consumption
across generations. Every next generation queries its own prefix.

## 3. Propositions and proofs

**P1 — The last-boundary query computes the specified open-turn projection.**
For every finite H and legal c, folding only the Boolean boundary rule on H through c gives
OpenAt(H,c). Proof by induction on c: the empty prefix is false. Extending by start makes both
sides true; end makes both false; any other fact changes neither side. These cases exhaust the
event abstraction. The proof is independent of the finite TLC bound.

**P2 — Live state agrees with its consumed prefix.**
Whenever phase=live, open=OpenAt(H,p). Initially this is P1's empty case. Append preserves the
existing prefix. Deliver preserves equality by P1's three cases. BeginRecovery leaves the live
antecedent false; TakeCut keeps it false. Recover establishes equality directly at c. No other
actions exist. While holding/querying, old open is not claimed to describe the new cutoff.

**P3 — A generation neither skips nor repeats live positions after its recovery base.**
When live, D is exactly the increasing sequence b+1,...,p. Initially it is empty. Recover resets
b=p=c and D to empty. Deliver appends precisely p+1. Other actions do not alter D or the consumed
position. Positions at or below c contribute through the query result, not through rerun handlers.
If H grows after TakeCut, the additional positions remain above c and are eligible for Deliver.
This establishes handoff safety under the delivery assumption, not eventual scheduling.

**P4 — A started recovery eventually returns to live, conditionally.**
Assume weak fairness for TakeCut and Recover, and that the abstract query completes successfully.
Holding continuously enables TakeCut, which eventually reaches querying. Querying continuously
enables Recover, which eventually reaches live. Append cannot disable either. Without query
completion/fair scheduling the claim is not made. Query failure instead follows the mechanism's
failed-load contract, outside this successful-read model.

| Proposition | Evidence form | Limit |
| --- | --- | --- |
| P1 | Induction above for arbitrary finite prefixes | Not a machine-checked proof; assumes the stated projection |
| P2, P3 | Transition-preservation arguments above; TLC invariants | TLC covers only its configuration; mechanism assumptions and Rust correspondence remain separate |
| P4 | Progress argument above; TLC temporal property | Conditional on the explicit fairness and successful-query abstraction |

## 4. Recovery query and cost obligation

| Required state | Scope and cutoff | Selection | Result | Recovery action |
| --- | --- | --- | --- | --- |
| open-turn Boolean | This branch, one fixed committed cutoff; permitted publishing facts included | kind in {turn/start, turn/end}, pick last | Only kind, at most one row | Start means open; end or no boundary means closed |

The query and the mathematical projection agree by P1. Neither notification history nor input
body is needed for this proposition. There is no pagination here; a multi-page recovery elsewhere
must keep the same cutoff and preserve related-event scope. Return c to Dispatcher, not a new
head read after the query. Recovery writes no fact for this projection.

The expected logical result is at most one scalar regardless of ended chat length. That is not
proof of constant SQL work. Validate kind-index selection, scans, decoded bytes and retained data
with growing historical size, separately from the logical proof. This example supplies no database
performance result.

## 5. Executable model and correspondence

TLC configuration bounds H to five events drawn from start/end/arrival/other. BeginRecovery can
repeat without a generation-count bound; the state space stays finite because old-generation
proof state is discarded. The history bound disables Append when full, so this model does not
prove properties of an indefinitely growing history. No symmetry reduction or additional state
constraint is used. Weak fairness is imposed only on the two recovery steps, not on external work.

Use Java 17 and `tla2tools.jar` from the official
[TLA+ 1.7.4 release](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4).
Keep the downloaded jar and TLC output outside the repository. From the repository root:

```sh
java -cp /absolute/path/tla2tools.jar tlc2.TLC -workers 1 \
  -metadir /tmp/dim-wait-tlc -config docs/design/examples/wait-model/WaitRecovery.cfg \
  docs/design/examples/wait-model/WaitRecovery.tla
```

Inspect the complete run: parsing, invariant checks, temporal checks and successful exhaustion,
not merely startup. Record tool version, configuration, counts and outcome in the PR/issue. A
counterexample is a design question before it becomes an implementation regression case.

| Model boundary | Implementation correspondence to inspect |
| --- | --- |
| Query result and Recover | [wait.rs](../../../crates/dim-agent-capabilities/src/wait.rs): `Wait::rebuild`, selecting kind with `QueryPick::Last` and returning `reply.through` |
| Deliver's Boolean transition | Same file: `Wait::consume`, start/end arms and irrelevant-fact cases |
| BeginRecovery, TakeCut, suffix delivery | Same file: `WaitBranchPlugin::apply`; [Dispatcher::consume](../../../crates/dim-agent-runtime/src/core/dispatcher.rs) and the query guarantee |
| Excluded pending arrivals and active calls | Wait's `pending`, `arrival` and caller cancellation; do not infer these properties from the Boolean model |

Existing [local execution flows](../../../tests/integration/tests/local_execution.rs) exercise
wait outcomes. They do not prove P1–P4 or establish all model/implementation correspondence. This
example does not change or assert a passing result for those tests.

## 6. Rule check and mechanism requirements

This models one branch Capability under its existing contract. No new service, event, writer,
side effect, persistence or assembly behavior is introduced. Placement and dependency rules remain
with Wait; query and delivery rules remain with their mechanism owners. There is no mechanism
requirement. The model's D and b exist only to express P3, not as proposed production state.

The [contract coverage and implementation experiment](wait-contract-experiment.md) extends this
projection with notification obligations and real Timeline/Wait service observations.
