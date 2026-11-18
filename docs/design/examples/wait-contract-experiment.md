# Wait: contract coverage and implementation experiment

Scope: connect Wait's Timeline obligations to observable implementation behavior.
Issue: [#1615](https://github.com/arcships/dim/issues/1615).

The [Wait owner document](../../architecture/rust-agent/plugins/Wait.md) remains the contract.
The [usage-context review](wait-usage-context.md) grounds the environment in actual callers,
producers and complete flows, and distinguishes broader test traces from reachable product traces.
This extends the [recovery argument](wait-argument.md) with a coverage inventory and a concrete
implementation experiment. It does not make the Boolean projection a complete Wait model.

## 1. Coverage comes from promises

| Obligation | Source / boundary | Argument and implementation evidence |
| --- | --- | --- |
| Only eligible arrivals during an open turn can wake a wait | WaitBranch events/lifetime | Notification argument below; real Timeline traces |
| End clears pending arrivals; reload preserves open status but drops old arrivals | Same owner contract | Notification argument plus recovery P1–P3; traces with end/reload |
| A task-specific call returns a matching task; a consumed arrival cannot be returned again | Wait tool and service contract | Notification argument; matching calls followed by draining calls |
| Recovery uses the last boundary and connects to later consumption | WaitBranch plus Dispatcher/query contracts | Existing P1–P4 and TLC model; real queries held across later boundary/arrival writes |
| Steer takes priority among currently pending arrivals, then the first matching task | Current `Wait::take` behavior, also referencing TS `takeMatchingInput` in its source | Explicit experimental selection policy below; not a new cross-plugin rule or a priority over future arrivals |
| One wait per branch; caller cancellation/drop releases the slot | Wait service contract | Existing concurrency/stop flow; the [usage-context experiment](wait-usage-context.md#4-corrections-to-model-scope) identifies missing generation cancellation as a violation of the existing reload protocol |
| Timeout bounds, error text and tool result format | Wait tool contract | Existing timeout-boundary flow; no mathematical timing guarantee here |
| Exact winner of simultaneous timeout, cancellation and arrival | No total priority specified by the owner contract | Do not infer one from a sequential experiment; an asynchronous model must permit the contracted outcomes |
| External task execution and cancellation | BackgroundExec or the corresponding task owner | Dependency on its notification contract; outside Wait's proof |

A contract inventory can expose missing specification as well as missing verification. In particular,
the pending-selection order is recorded here as the implementation policy under test, not silently
promoted to an owner-independent requirement. Full Wait verification still needs asynchronous call
admission/completion arguments and the relevant implementation correspondence.

## 2. Notification problem

Assume ordered delivery, successful append and reload, and calls made after delivery settles.
A reload creates a fresh Wait generation. External work is represented only by valid tagged notices;
the experiment starts at the Timeline writer boundary, not at shell execution or user input validation.

Let H be the sequence of actions Start, End, Task(a), Task(b), Steer, Plain, Consume, Delete,
Reload, Take(a) and Take(b). Plain is an untagged non-steer input. Consume is the model reading the
latest Task or Steer input, and Delete is that input being deleted instead of read; each retires the
input it names. An arrival counts only once its input is released, and the model may only read or
delete an input that was released, so the two are one position here.
Each arrival occurrence has its position in H as identity; two notices with the same task id are
still two occurrences. Let C contain the positions already returned to callers.

Define the eligible pending positions Q declaratively:

```text
Q(H,C) = increasing positions i such that:
  H[i] is an arrival,
  the last Start/End before i is Start,
  no End or Reload occurs after i,
  no Consume or Delete of i's input occurs after i,
  i is not in C.
```

An input already given to the model is outside Q. So is the input that opened the turn: it is
released before Start, and admission starts at Start.

A reload is a clearing boundary for arrivals, but not a closing boundary for the turn.
A call selects the first Steer in Q if any; otherwise it selects the first matching task in Q.
An unspecified task matches either id. A call with nothing to select returns already_notified when it
names a task the model read in this turn, and timeout otherwise. A Delete records no task, and End
or Reload clears the record. This last statement concerns a zero-timeout service call with
already-settled inputs, not the tool's positive timeout range or a race with a concurrent arrival.

**N1 — Selection is supported by a live, eligible occurrence.** Q's defining predicates exclude
closed-turn arrivals, arrivals cleared by end/reload and previously consumed occurrences. Selection
is a member of Q, so a returned task matches the request unless the result is a steer.

**N2 — One returned occurrence cannot be returned again.** Add its position to C. All future Q
sets exclude it, including after reload. Repeated notices with the same task id remain distinct;
this is not a promise of deduplication by task id.

**N2b — An input the model read is never returned.** A Consume of the input at position i retires
i. Q excludes it from then on, so a wait that starts after the model read the input waits for what
comes after it, instead of returning an arrival the model already has.

**N2c — A read notice answers its own task without consuming an arrival.** A call naming a task
whose notice the model read in this turn returns already_notified for that task, because the notice
it names will not come again. It consumes no arrival and spends no record: the answer is about the
task, so the queue and the record are as the call found them, and every later call naming that task
answers the same way until the turn ends. The record is made by Consume alone, so a Delete of the
same input leaves the call to timeout, and End or Reload clears the record before the call is made.
The record is read where the call polls, before it registers and again after each wake, so a notice
read while the call sleeps does not end that sleep. A selectable occurrence still wins: when a
steer, or another notice for the named task, is queued, the call takes it and ends as steer or
notification instead of answering already_notified.

**N3 — Recovery preserves the admission rule without restoring old notifications.** The recovered
open flag is OpenAt(H,c), by P1 of the recovery argument. The new generation's pending sequence is
empty. A suffix arrival is admitted exactly when the latest boundary preceding it is Start;
pre-cut arrivals are excluded by the Reload clearing boundary. Subsequent End clears the suffix.

A representation relation to the implementation is: `open` equals the last-boundary predicate,
`arrivals` holds the released arrival of each input id that is neither consumed nor deleted, and
`pending` holds the ids still in the queue, in release order; `notified` holds the tasks whose notice
the model read in this turn. Start changes only the admission predicate; End clears `open`,
`arrivals`, `pending` and `notified`; a release appends only when admitted; Consume retires one id
and records its task, Delete retires one id and records nothing; taking removes one id from `pending`
and leaves it registered, since the model reading it afterwards still records what it was;
successful reload starts with empty pending and an empty record and reconstructs open.
These cases preserve the relation under the stated sequential assumptions. This is a manual
argument about the specified transitions, not a machine-checked proof of Rust or its locks.

## 3. Executable correspondence experiment

[wait_protocol.rs](../../../tests/integration/tests/wait_protocol.rs) mounts the actual WaitBranch
plugin with the actual Timeline and Dispatcher. It neither replaces Wait with a mock nor reads
private state. The host supplies an idle LoopState instead of the agent loop, so the test writer
controls all facts; it writes each input's release, consumption and deletion itself, and no model
execution runs in the test. A test writer supplies facts through Timeline, and calls the public
Wait service. A separate history predicate computes Q; it does not maintain a duplicate mutable
`pending` queue.

The experiment enumerates all 1331 length-three traces over eleven actions, then fourteen longer
traces covering pending selection, reload followed by new arrival, turn boundaries, repeated task
ids, an input the model read or that was deleted, and the task-specific call for each task.
Every trace includes a final task-specific call and enough unfiltered calls to drain the queue and
observe timeout. Mid-trace Take(a) checks that a nonmatching arrival remains available, and the
longer traces use Take(b) so a task whose notice was read, or deleted, is asked for by id.
Repeated Start/End combinations over-approximate the selected contract; these are not claims that
the full product's turn producer emits every such sequence.

Two additional schedules hold the actual recovery query after its cutoff has been chosen, append
a new boundary and a released steer, and release the query. One opens a previously closed turn; the other
closes an open turn. The observable result must follow the post-cut boundary, with no repeated
steer. These target real query/Dispatcher handoff rather than assuming it in the test oracle.

| Model term | Real boundary | Observation |
| --- | --- | --- |
| Arrival and turn boundary | Timeline append → Dispatcher → `Wait::consume` | Subsequent public wait result |
| Release and Consume | `input/applied`, `input/consumed` or `input/deleted` append → Dispatcher → `Wait::consume` | An arrival the model read answers no later wait; a deleted one does not answer `already_notified` |
| Reload | Restart the mounted WaitBranch generation → `Wait::rebuild` | Old arrivals absent; new arrivals admitted according to restored turn status |
| Take | Public `Wait::wait` → arrival selection | Result reason/task id; elapsed milliseconds intentionally ignored |
| Fixed-cut handoff | Held store query, concurrent Timeline writes, then release | Open/closed suffix changes the returned result exactly as specified |

Run from the repository root:

```sh
cargo test --locked -p dim-agent-integration-tests --test wait_protocol -- --nocapture
cargo test --locked -p dim-agent-integration-tests --test local_execution wait_
```

Run the separate TLC command in the recovery example for P2–P4. This experiment does not consume
TLC traces, claim exhaustive interleavings of Rust, or verify persistent SQLite query performance.
Equal-valued notices are observationally indistinguishable: the test checks returned values and
counts, not inaccessible occurrence identities. Run counts, failures and conclusions belong in the
PR. A discrepancy must be classified as a specification gap, model error or implementation error
before changing either side to make the test pass.
