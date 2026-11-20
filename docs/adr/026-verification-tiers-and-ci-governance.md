# 026: Verification tiers, test coverage and CI governance

Decision status: Proposed.

## Context

Repository verification grows monotonically when every discovered defect or new check is appended
directly to the pull-request gate. Over time, two distinct concepts get conflated:

- **Repository verification coverage**: every check, formal model, suite, benchmark and acceptance
  procedure the repository maintains.
- **Active pull-request gate**: the bounded subset that must complete synchronously before a change
  merges.

When those two are treated as identical, slow suites, cross-platform checks, compile-time rebuild
loops and brittle text scans all pile onto one flat gate. Raising timeouts when jobs approach the
ceiling hides the degradation until queues, compiler-cache eviction and mutual cancellations erode
both contributor turnaround and `main` signal quality.

## Decision

### D1. Verification separates into three operational states; tiers (L0–L4) map assets to states

Every verification asset has an explicit operational state that governs its lifecycle and CI cost:

1. **Active gate**: runs automatically on pull requests under explicit wall-clock budgets, guarding
   high-frequency contracts and the affected dependency closure.
2. **Retained (on-demand & sweep)**: stays in the repository without gating ordinary pull requests.
   Runs when the asset's own source files change (for example, formal model edits), on scheduled
   full sweeps, before release, when diagnosing or reproducing a failure, or when a design/protocol
   review calls for it — with the tier table's "Default state & trigger" column authoritative for
   each tier. Retained Rust tests remain compiled (for example, consolidated with `#[ignore]`) so
   they do not bitrot; external-toolchain checks (TLC/Java, local tmux E2E, standalone profiling
   scripts) stay out of everyday build toolchains.
3. **Dev / exploratory**: temporary probes or fine-grained local tests created while developing or
   debugging. Ablated before merge: enduring contract coverage moves into the active gate (D3),
   reusable diagnostic reproducers move into Retained, and throwaway scaffolding is removed.

Verification tiers (L0–L4) map each class of asset to its default operational state and trigger:

| Tier | Question answered | Scope | Default state & trigger |
| --- | --- | --- | --- |
| **L0 — Design & formal** | Do abstract state-machine, protocol and handover invariants hold? | Formal models (e.g. TLA+ specifications and `.cfg` properties) | **Retained**: (a) triggered on formal model file changes (with tool cost disclosed under D4 if wired into CI), and (b) enforced via PR checklist/review whenever an implementation change alters a modelled handover, recovery or ordering guarantee. Not run on ordinary implementation PRs. |
| **L1 — Static form & contract** | Are formatting, dependency boundaries, ADR immutability, public schemas/wire types and license/advisory policies intact? | Formatters, linters, pure static form/structure checks (including ADR freeze checks on every PR / ADR edit), frontend typecheck/lint, source-derived wire/schema contracts, dependency advisory/license scans | **Active gate**: fast fail-first on every PR, scoped to the changed surface (dependency scans only on manifest, lockfile or policy changes). |
| **L2 — Business flows** | Do modified modules and their transitive consumers preserve behavioral contracts? | Unit and in-process or loopback flow tests (plus runtime-assembled structural checks that require compiling the workspace) | **Active gate** on the primary high-throughput platform for the **reverse-dependency closure** of changed crates (expanding to the full workspace when foundational crates or workspace-wide configuration change). Heavy historical, soak or low-frequency flows belong in **Retained**. |
| **L3 — Real environment & cross-platform** | Do real OS primitives, process/plugin wiring, terminal interaction and external protocol boundaries work? | OS-native boundary slices, real daemon + plugin runtime flows, interactive terminal E2E suites, external registry acceptance | **Split**: narrow OS-native boundary slices run in the **Active gate** only on PRs touching platform/client code; full cross-platform runs and external-registry acceptance are **Retained** (asynchronous `main`/scheduled sweeps, pre-release or on-demand); interactive terminal E2E is **Retained (local-only)** — run locally on demand or before release, **never** invoked by a CI workflow. |
| **L4 — Quality, performance & release** | Have model quality, resource budgets, executable layout or distribution artifacts regressed? | Evaluation harnesses, memory/startup measurements, native code-layout builds, multi-platform release builds and registry/image acceptance | **Execution is always Retained** (scheduled, manual or release train only — never gates ordinary feature PRs). Changes to an L4 harness itself run the harness's own L1/L2 checks in the **Active gate** on that PR. |

Formal models (L0) verify that an abstract state machine satisfies safety and conditional liveness
invariants within a finite state space; they do not compile or test implementation code. Ordinary
implementation changes rely on L2 flow tests to guard that code still refines the model's actions.
If a code change alters a modelled handover or ordering guarantee, it is a design change and returns
to L0 through review before merging.

### D2. Documentation guides and states semantic contracts; automated checks enforce form, not prose semantics

Documentation exists to guide contributors and state semantic contracts. Automated checks over
documentation and source conventions enforce only **deterministic, low-false-positive form and
structural invariants**:

- **Pure static form checks (L1, zero business compilation)**: lifecycle/immutability form (such as
  accepted-ADR freeze checks), link and template structure, and manifest- or AST-derived structural
  checks (such as crate/module dependency boundaries and source-parsed type declarations) that do
  not compile business runtime crates and do not parse natural-language prose.
- **Runtime-assembled structural projections (workspace-coupled, alongside L2)**: deterministic
  code-to-document projections that inherently require compiling and assembling the runtime (such
  as a live public service catalog) run on the workspace-coupled build/verify path rather than
  blocking pure-static docs, frontend or script PRs.
- **Keep generated snapshots minimal**: commit and freshness-check a generated file only when
  downstream code consumes it (e.g. cross-language type declarations) or when it is a concise,
  actively referenced public summary. Fine-grained internal dependency graphs computed for boundary
  checks are verified live in memory and generated on demand rather than committed to Git as
  conflict-prone snapshots.
- **Out of scope for automated checks**: semantic correctness of prose, design quality, and
  judgement calls such as whether an edit alters a modelled invariant. Avoid introducing new brittle
  cross-language text scanners that try to infer semantics from raw literals and rely on growing
  manual allowlists (existing allowlist-backed scanners are kept only until replaced by types,
  boundaries or behavioral tests rather than imitated); enforce invariants through types,
  boundaries, review checklists, L0 models and L2 behavioral tests instead.

### D3. Test coverage follows lowest flow/contract layer, single owner, release-safe injection and build hygiene

Coverage quality is measured by contract protection per unit of cost, not by test count at the top
layer.

- **Lowest flow or contract layer that proves the contract**: assertions about private modules,
  helpers or single functions belong in the nearest flow or contract test rather than standalone
  unit tests, while state transitions, rendering rules, validation errors and edge matrices stay
  out of process-spawning, PTY or binary-copying E2E tests. End-to-end suites cover wiring and real
  OS interaction on representative paths rather than exhaustive combinations.
- **One owning layer per behavior**: once a lower layer (or a dedicated suite such as the local
  terminal E2E suite) proves a behavior, higher layers verify only connectivity and do not duplicate
  its failure matrix. Migrating coverage to a better carrier requires retiring or demoting the
  superseded active CI case in the same change or tracked migration.
- **Release-safe runtime injection instead of recompiling**: test variants (release stamps, build
  IDs, clocks, endpoints, fault injection) must be injectable at runtime or test boundaries without
  flipping build-time flags that recompile upstream crates in CI. **Production-safety bound**: every
  test-only override must be unreachable in release builds (compiled only under debug/test builds)
  or confined to test-harness entry points so test injection never widens what a shipped binary
  accepts.
- **Treat test executables as build artifacts**: consolidate integration tests into a bounded number
  of balanced entry binaries per crate to avoid linker and compiler-cache amplification from dozens
  of single-file executables, while splitting oversized binaries that create single-process
  scheduling tails or linker pressure.

### D4. Active gates operate under recorded budgets and physical light/heavy isolation

- **Recorded wall-clock and resource budgets**: every synchronous active-gate tier declares a
  recorded latency and resource budget (maintained alongside runner measurements in
  `docs/architecture/build.md` and tracking issues, not hardcoded as immutable constants in this
  ADR). Approaching or hitting a budget is a regression fixed by narrowing scope, optimizing slow
  tests, improving parallelism or demoting cases to Retained — **never** by raising job or target
  timeouts alone.
- **Cost disclosure for gate additions**: any change that adds a required PR check or widens path
  selection must state its incremental runtime, added compile dependencies and target runner pool.
- **Physical isolation of light checks from heavy builds**: pure static repository checks must not
  depend on compiling business runtime crates. Docs, frontend, script and static checks run in
  lightweight jobs and never contend for heavy build or scarce platform runner pools.

### D5. Impact-scoped routing and platform minimalism require scheduled full sweeps and periodic pruning

- **Impact-scoped routing with sweep prerequisite**: both pull requests and pushes to `main` route
  by changed paths, and `main` pushes run to completion without cancelling earlier ones. Narrowing
  `main` pushes is permitted **only while an active scheduled full sweep is running** to catch
  cross-group drift; a sweep failure must have defined triage ownership and commit-range bisection
  back to the regressing merge on `main`.
- **Explicit path classification**: new top-level repository directories and non-code assets must
  be classified explicitly in the change router rather than silently falling through to the
  most expensive full-workspace fallback.
- **Scarce platforms run platform-native boundaries only**: full workspace compilation and
  platform-neutral L1/L2 verification run on the highest-throughput primary platform (Linux).
  Secondary OS pools (macOS, Windows) run only checks covering platform-specific code paths
  (`cfg(target_os = ...)`), native OS APIs or packaging boundaries on pull requests, verified
  mechanically against the source rather than maintained as an unchecked manual list.
- **Periodic telemetry and pruning**: CI health (queue p90, execution p90, cache write/eviction
  volume, slowest test targets, zero-match dead steps and cancellation waste) is reviewed
  periodically to retire obsolete checks and demote emerging long tails to Retained.

## Consequences

- Every new test, static check or formal model declares its operational state (Active / Retained /
  Dev), tier (L0–L4) and owner before wiring into CI.
- Until this ADR is accepted and its follow-up tooling changes land, `AGENTS.md` and
  `docs/architecture/` remain the authoritative reference for today's commands and committed
  snapshots; once aligned, `AGENTS.md` (`## Testing`) can consolidate its testing rules onto this
  ADR so the rule is stated once.
- Pure static form checks are decoupled from workspace-coupled tooling so docs, frontend and script
  PRs stay fast.
- Test-only build flags move to release-unreachable runtime/harness injection, eliminating
  cascading CI recompiles without weakening production boundaries.
- Concrete workflow changes, runner migrations, scheduled sweeps, test-binary consolidation and
  numeric budgets live in `docs/architecture/build.md`, `docs/release/README.md` and tracking
  issues rather than here.

## Non-goals

- Lowering overall defect detection or deleting coverage without an owning replacement at a more
  appropriate layer or operational state.
- Freezing numeric latency targets, runner hostnames, workflow syntax or crate-by-crate routing
  tables in an immutable ADR.
- Replacing design review or code review with automated checks.

## Supersession

Does not supersede any prior ADR. Extends [ADR 008](008-documentation-ownership.md) by separating
verification and CI governance principles (here) from numeric budgets and current CI implementation
(`docs/architecture/build.md`, `docs/release/README.md`) and delivery status (issues).

References: [ADR 008](008-documentation-ownership.md),
[Timeline finite model checking](../design/examples/capability-models/README.md),
[Build environment](../architecture/build.md),
[Terminal end-to-end suite](../architecture/tui-e2e.md),
[#1938](https://github.com/arcships/dim/issues/1938).
