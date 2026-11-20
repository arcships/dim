# 025: Verification tiers, test coverage and CI governance

Decision status: Proposed.

## Context

Repository verification tends to grow monotonically when every discovered defect or new check is
appended directly to the pull-request gate. Over time, two distinct concepts get conflated:

- **Repository verification coverage**: every check, formal model, suite, benchmark and acceptance
  procedure the repository maintains.
- **Pull-request gate execution**: the bounded subset that must complete synchronously before a
  change merges.

When those two are treated as identical, slow suites, cross-platform checks, compile-time rebuild
loops and pure static scans all pile onto one flat, serial gate. When execution approaches the job
limit, raising timeouts hides the degradation until queues, cache eviction and mutual cancellations
undermine both contributor turnaround and `main` signal quality.

## Decision

### D1. Verification is tiered by change nature and feedback cycle

Verification assets are assigned to five tiers according to what failure class they catch, what
triggers them and how fast they must answer:

| Tier | Question answered | Scope | Trigger |
| --- | --- | --- | --- |
| **L0 — Design & formal** | Do the state-machine, protocol and handover invariants hold in the abstract? | Formal models (e.g. TLA+ specifications and `.cfg` properties), ADR immutability checks | **Event-driven on design/contract change**: when a formal model, protocol/event contract, recovery boundary or handover invariant changes. Not run on ordinary implementation PRs. |
| **L1 — Static & contract** | Are formatting, dependency boundaries, public schemas/wire types, static source rules and license/advisory policies intact? | Formatters, linters, pure static repository scans, frontend typecheck/lint, wire/schema contracts, dependency advisory/license checks | **Every PR**, scoped to the changed surface (dependency scans only on manifest, lockfile or policy changes). Fast fail-first gate. |
| **L2 — Affected business flows** | Do the modified modules and their direct dependents preserve behavioral contracts? | Unit tests and in-process or loopback flow tests for the affected crates and their downstream consumers | **Every PR** on the primary high-throughput platform; expands to the full workspace only when shared foundational crates or workspace-wide configuration change. |
| **L3 — Real environment & cross-platform** | Do real OS primitives, process/plugin wiring, terminal interaction and external protocol boundaries work? | OS-native boundary slices, real daemon + plugin runtime flows, interactive terminal E2E suites, external registry acceptance | **Split by cost**: narrow OS-native boundary slices run on PRs that touch platform/client code; full cross-platform runs and external-registry checks run asynchronously on `main` or on a schedule; interactive terminal E2E runs locally on demand and before release. |
| **L4 — Quality, performance & release** | Have model quality, resource budgets, executable layout or distribution artifacts regressed? | Evaluation harnesses, memory/startup measurements, native code-layout builds, multi-platform release builds and registry/image acceptance | **Scheduled (nightly), manual dispatch or release train only**. Never gates an ordinary feature PR. |

Formal models (L0) verify that an abstract state machine satisfies safety and conditional liveness
invariants within a finite state space; they do not compile or test implementation code. Ordinary
implementation changes rely on L2 flow tests to guard that code still refines the model's actions.
If a code change alters a modelled handover or ordering guarantee, it is a design change and returns
to L0 before merging.

### D2. Test coverage follows lowest-layer, single-owner and build-hygiene rules

Coverage quality is measured by contract protection per unit of cost, not by test count at the top
layer.

- **Test at the lowest layer that proves the contract**: logic, state transitions, rendering rules,
  validation errors and edge-case matrices belong in component, state-machine or in-process flow
  tests — never in process-spawning, PTY or binary-copying E2E tests. End-to-end suites cover
  wiring and real OS interaction on representative happy and failure paths rather than exhaustive
  combinations.
- **One owning layer per behavior**: once a lower layer (or a dedicated suite such as the local
  terminal E2E suite) proves a behavior, higher layers verify only connectivity and do not duplicate
  its failure matrix. Migrating coverage to a better carrier requires retiring or disabling the
  superseded CI case in the same change or tracked migration.
- **Inject testability at runtime boundaries instead of recompiling**: test variants (release
  stamps, build IDs, clocks, endpoints, fault injection) must be configurable at the binary or test
  boundary without flipping build-time environment variables that recompile upstream crates in CI.
- **Treat test executables as build artifacts**: organize integration tests into a bounded number
  of balanced entry binaries per crate to avoid linker and compiler-cache amplification from
  dozens of single-file test executables, while splitting oversized test binaries that create
  single-process scheduling tails or linker pressure.

### D3. CI gates operate under explicit budgets and light/heavy isolation

- **Hard latency and resource budgets**: every synchronous tier maintains an explicit wall-clock
  ceiling (target: L1 ≤ 2 min; PR end-to-end median ≤ 8 min, p90 ≤ 15 min; OS-native PR slice
  ≤ 3 min). Approaching or hitting a timeout is a regression to fix by narrowing scope, optimizing
  slow tests, improving parallelism or moving work to L3/L4 — **never** by raising job or target
  timeouts alone.
- **Cost disclosure for gate additions**: any change that adds a required PR check or widens path
  selection must state its incremental runtime, added compile dependencies and target runner pool.
- **Physical isolation of light checks from heavy builds**: pure static repository checks (boundary,
  link, literal, packaging metadata) must not depend on compiling business runtime crates. Docs,
  frontend, script and static checks run in lightweight jobs and never contend for heavy build or
  scarce platform runner pools.

### D4. Impact-scoped routing is the default, backed by scheduled full sweeps

- **Consistent routing on PRs and `main`**: both pull requests and pushes to `main` select only the
  groups their changed paths affect. `main` pushes run to completion without cancelling earlier
  `main` pushes, so every merged commit produces an honest pass/fail signal without non-code or
  frontend merges starving backend runners.
- **Scheduled full sweeps cover cross-cutting drift**: periodic scheduled runs (and release-train
  verification) execute the full cross-platform and cross-workspace matrix so implicit coupling
  cannot accumulate unnoticed.
- **Explicit path classification**: new top-level repository directories and non-code assets must
  be classified explicitly in the change router rather than silently falling through to the
  most expensive full-workspace fallback.

### D5. Scarce platform pools verify platform-native boundaries only

- **Primary platform for platform-neutral work**: full workspace compilation and platform-neutral
  L1/L2 verification run on the highest-throughput, lowest-queue runner pool (Linux).
- **Minimal, mechanically checked secondary OS slices**: scarce OS pools (macOS, Windows) run only
  checks that exercise platform-specific code paths (`cfg(target_os = ...)`), native OS APIs or
  target packaging boundaries on pull requests. The platform-native selection is verified against
  the source rather than maintained as an unchecked manual list.
- **Periodic telemetry and pruning**: CI health (queue p90, execution p90, cache write/eviction
  volume, slowest test targets, zero-match dead steps and cancellation waste) is reviewed
  periodically to retire obsolete checks and trim new long tails.

## Consequences

- Adding a new test, static scan or formal model requires declaring its tier (L0–L4) and owner
  before wiring it into CI.
- Pure static checks are decoupled from workspace-coupled tooling so frontend, docs and script
  changes stay fast.
- Build-time flags that vary only for testing are replaced by boundary/runtime injection so CI
  never triggers cascading crate recompiles.
- Test binary layout and cache footprint become reviewable engineering concerns alongside runtime
  code.
- Concrete workflow changes, runner migrations, test-binary consolidation and slow-test cleanups
  are tracked in separate implementation issues rather than here.

## Non-goals

- Lowering overall defect detection or deleting coverage without an owning replacement at a more
  appropriate tier.
- Prescribing specific CI vendor syntax, runner hostnames or crate-by-crate path tables (those live
  in workflow files, architecture guides and issues).
- Replacing code review or design review with automated checks.

## Supersession

Does not supersede any prior ADR. Extends [ADR 008](008-documentation-ownership.md) by separating
verification policy rules (here) from current CI implementation details (`docs/architecture/build.md`,
`docs/release/README.md`) and delivery status (issues).

References: [ADR 008](008-documentation-ownership.md),
[Timeline finite model checking](../design/examples/capability-models/README.md),
[Build environment](../architecture/build.md),
[Terminal end-to-end suite](../architecture/tui-e2e.md),
[#1938](https://github.com/arcships/dim/issues/1938).
