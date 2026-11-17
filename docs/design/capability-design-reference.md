# Capability design argument reference

Scope: optional reasoning techniques for the [design guide](capability-design-arguments.md).
Issue: [#1615](https://github.com/arcships/dim/issues/1615).

Read the relevant section when a design needs it; this is not an additional checklist or a
requirement to supply an argument. Use a technique only to resolve a specific question that ordinary
contract review and relevant flow tests leave unclear. Event, query and lifecycle semantics remain
in their existing contracts. The [responsibility review](capability-document-review.md) identifies those sources.

## 1. Name the role, not an ambiguous owner

Use these distinctions when writing an argument; their underlying semantics remain in the linked contracts.

| Role | Meaning and source |
| --- | --- |
| State owner | The part that changes a particular business state; other parts may derive their own views from the same facts ([time bus §2](../architecture/rust-agent/time-bus.md#2-事件是总线上带序号的事实)) |
| Event contract maintainer | Defines the meaning and compatibility of the event type; this is not exclusive authority to append it ([ADR 001, Events and ownership](../adr/001-harness-foundation.md#events-and-ownership-issue-2)) |
| Writer | Produces an event satisfying that contract and the applicable work/port authority; several writers may produce the same type (same source) |
| Consumer | Declares the facts its decisions need, including query dependencies; the set of consumers is open ([principles §3](../architecture/rust-agent/principles.md#3-正交)) |
| Work owner | Starts, cancels and settles its own work; a new consumer does not acquire historical work merely by reading it ([time bus §2, §5](../architecture/rust-agent/time-bus.md)) |

Writer unload does not invalidate or delete facts. Unknown extension facts remain data, not
instructions to install code or run effects (ADR 001). A known-consumer list is an illustration,
not an event validity condition. No correctness argument may assume that no new conforming writer
or consumer can be added unless a specific contract actually supplies that restriction.

## 2. Write the problem before choosing the implementation

Start from the change's intended use and the authoritative contracts. Identify the caller, event
producers, admission boundary and lifecycle responsibilities through their public contracts;
use the selected profile and existing complete flows to check that those assumptions represent
actual use. Code and experiments are evidence of behavior, not authority to redefine a contract.
If behavior conflicts with an existing requirement, classify it as a compliance defect before
proposing a new policy. A new integration states the guarantees it relies on without enumerating
all future plugins.

From that context, identify the obligations affected by the change and the owner of each.
A compact table can link obligation, contract source, argument and implementation observation;
prose is sufficient when that is clearer. Coverage is relative to these declared obligations,
not a claim that every possible business need has been discovered. Do not add a requirement
solely because a speculative trace can be constructed: establish that the trace is permitted
by the environment and violates an agreed promise. Broader robustness experiments may still be
useful, but their inputs are not evidence of product reachability.

For a Timeline argument, the subject is the part's contract with the Timeline: the facts it
consumes and produces, their effect on its business state, and recovery and consumption handoff.
It does not prescribe the size, internal organization or execution algorithm of a Capability.
The definition of a Capability remains [principles §1](../architecture/rust-agent/principles.md#1-插件部件与-capability-有界).
A transition table describes this contract; it is not a requirement to split a feature into tiny
state machines or implement a pure reducer.

Plugin-specific execution design owns the internal algorithms of external calls, processes,
resource management and external systems. Those may have their own models and proofs, but this guide does not require
proving them as part of every Timeline argument. At the boundary, name only the inputs and
guarantees needed by the proposition. For example, a claim about a terminal fact may depend on
the work owner's settlement guarantee; link that guarantee rather than modeling its entire
execution. If that guarantee is not established, keep the dependency explicit instead of claiming
an unconditional result. A Timeline proof is not a proof of all plugin behavior.

This scope does not exclude the part's admission, in-flight work settlement or generation boundary.
Apply the existing [lifecycle contract](plugin-contract.md#7-lifecycle-of-a-part) when deriving
obligations; distinguish its old-work settlement from the new-state query. "Caller-owned future"
or "external execution" is not an exemption. An obligation may have a separate argument or owner,
but must remain accounted for; excluding it from one model does not discharge it.

If a transition model helps answer the question, define the relevant state space S, its initial
condition Init, the input/event domain E and the allowed transition relation Next. A useful notation for one transition is:

```text
(relevant business state, consumed fact or declared boundary input)
    -> (new business state, facts to append, declared work requests)
```

This is design notation, not a new runtime API. Choose the abstraction for the proposition:
append, publication and consumption need separate steps only where their distinction affects
that proposition. Internal execution can remain abstract behind its declared contract. For
example, a cancellation request and a settled result cannot be treated as the same boundary
input when their difference changes which terminal facts are allowed.

## 3. When making a mathematical argument

For a safety invariant I, give both obligations:

```text
Init(s) implies I(s)
I(s) and Next(s, s') imply I(s')
```

Check every transition admitted by the stated environment that can affect the proposition,
including relevant ignored inputs and failure outcomes. A temporal
property may need a small history variable in the model; that does not require production code
to retain a history copy. Do not strengthen assumptions solely to exclude an inconvenient path.

For an eventual outcome, state what must eventually happen in the environment: scheduling,
storage completion, resource release, or an external response. Explain why those assumptions
imply progress. A system permitted to crash forever cannot promise eventual successful settlement.

For composition, name the guarantees the part needs from its neighbors and show how they are
provided. Proving two parts independently does not prove their handoff. Consider a new conforming
writer, a consumer disappearing, and a late result where those affect the proposition. Do not model
arbitrary plugins as authorized writers of another part's terminal facts.

## 4. When reasoning formally about recovery

Define ExpectedRecovery(H, c) from the owning business contract for history H at cutoff c.
Then establish:

```text
Recover(Query(H, c)) = ExpectedRecovery(H, c)
```

The equality may be observational equivalence if the two states use different representations;
name the observations. It does not require equality with all pre-unload memory. Goal pauses on
reload, Wait discards old arrivals, and resource owners settle their unfinished work according to
their own contracts. First installation may have different obligations from replacing an existing
generation; state them rather than adopting historical work implicitly.

Derive the selection and returned fields from the required result. Explain complete correlation
keys, why finished objects are excluded, and why still-effective objects are included. All reads
in a recovery use the same cutoff and relevant ancestor cuts; continuation narrows the outer
result page, not related-event scope ([query §2–3](timeline-property-query.md)). Query errors are
not empty results. The Dispatcher handoff is a separate obligation ([plugin contract §5.2](plugin-contract.md#52-consuming-facts)).

A simple mathematical definition over history is allowed as the proof reference; it is not a
production replay algorithm. If ordered processing is necessary, select required fields and
consume pages without accumulating historical events. Persistence choices follow the
[saving boundaries](owner-state-indexed-reads.md); no per-event snapshot protocol follows from this equation.

Logical correctness does not prove indexed execution cost. State what should scale with current
objects rather than ended history, and measure candidate scans, decoded bytes and retained data
on the implementation under the [resume acceptance](resume-read-design.md#8-验收).

## 5. Evidence and implementation correspondence

| Evidence | Information needed to support this claim | What it does not establish |
| --- | --- | --- |
| Mathematical argument | Definitions, assumptions, proposition, initial case and preservation or progress argument | Machine checking or correctness of source code |
| Machine-checked proof, for example Lean | Exact theorem, definitions, assumptions/axioms, tool version and reproducible command | Correctness of a different model or its Rust implementation |
| TLA+ with TLC | Model, finite configuration, checked invariants/properties, fairness, state constraints and reproducible command | An unbounded theorem from a bounded run |
| Implementation flow test | Model actions mapped to real public boundaries and observable outcomes | All executions beyond those exercised |
| Performance measurement | Workload, current objects versus historical size, scan/decode/retention measurements | A logical safety or liveness proof |

Record outstanding obligations and their effect on the claim. Keep execution results, counterexample
investigation and delivery progress in the issue/PR. Tool references:
[TLC](https://docs.tlapl.us/using:tlc:start) and
[Lean proof validation](https://lean-lang.org/doc/reference/latest/ValidatingProofs/).
No design is required to use a particular tool or all evidence forms. Mathematical reasoning may
be a short case analysis; use a machine-checked proof or bounded model where it helps establish
the claim. Scale the document to the affected contract: unchanged obligations can link existing
design material. Omit techniques and sections that do not help the question at hand. Retain the sections
required by AGENTS.md.
Do not split a Capability or introduce runtime state solely to fit the template or proof tool.

Map the design's actions and observation points to implementation entry points. Classify a
counterexample against the contract before writing a regression expectation: it may expose a
model mistake, an implementation defect or a genuinely unspecified requirement. A characterization
of defective behavior must not become a test requiring that defect. Reuse complete flow tests to
check the boundary with real callers as well as the local model. Code review checks whether locks, awaits, queries and
effects preserve the modeled boundaries; model correctness alone does not establish that mapping.

The [WaitBranch example](examples/wait-argument.md) gives a scoped mathematical argument and
executable model of recovery/consumption. Its assumptions and excluded behaviors are part of the claim.

The [Wait contract experiment](examples/wait-contract-experiment.md) demonstrates a coverage
inventory and a history-based oracle compared with actual public service results.
