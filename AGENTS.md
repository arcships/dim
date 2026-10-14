# AGENTS.md

This repository is Dim: a Rust coding agent (Agent v4), its App Server and the native `dimcode` CLI. See [README](README.md) and the [documentation index](docs/INDEX.md).

## Working conventions

- Set up the toolchain and workspace according to the [build environment](docs/architecture/build.md); it applies to both local development and CI.
- Write code, comments and commit messages in English. Documentation may be written in English or Chinese; translations live under `docs/zh/`, mirroring `docs/`.
- Illustrate terminal UI and interaction changes with ASCII diagrams.
- The corresponding design document is authoritative. When a requirement conflicts with the design, update the document and get confirmation first.
- After each design or implementation, do an ablation review: check whether every change is necessary and prefer removing abstractions, wrappers, intermediate state and extra branches that do not serve the requirement. Where a simplification is plausible, try it and confirm the result still satisfies the requirement and interface contracts within the affected scope. For structure that is required, state its purpose; do not deliberately revert fixes or manufacture test failures to prove necessity. If nothing can be removed, say so instead of inventing experiments.
- Process serves delivery; keep effort proportional to the complexity and risk of the change. Handle ordinary changes directly and only delegate complex work that can progress independently. Tracking issues individually does not mean a separate agent, report or full verification run per issue.
- Development and tests use an isolated `--home` (or `DIMCODE_HOME`); never touch the product home `~/.dimcode`.
- This is a public repository. The private account plugin, its account realm and token flows, and any credentials stay out of it; see the [managed provider boundary](docs/architecture/development.md#managed-provider-boundary).
- Review correctness, security, compatibility and unnecessary abstraction before committing. Commit complete, reviewable units on their declared parent, with their documentation and observable-behavior tests. Never commit secrets or scratch experiments.

## Data and interface conventions

- Each kind of internal data has one type definition. The producer guarantees the contract; consumers use it directly without re-validating, converting or filling in fields.
- Parsing, validation and format conversion happen at the boundary where external data enters the system (user input, network responses, persisted files). Afterwards data is passed as explicit types and is not reprocessed layer by layer.
- When internal data does not match its contract, fix the producer, the type definition or the call flow. Do not mask the problem in consumers with field aliases, defaults, casts or conversion functions.
- Do not add read/parse/normalize wrappers for data of a known type; access fields and call interfaces directly. Such a function must correspond to a real external data boundary or business transformation.
- Missing values and recovery behavior must have a clear business meaning; do not introduce them because of uncertainty about what upstream returns.
- `ToolResult.content` is the only text the model can read. Handles, cursors and state the model needs later must be written into `content`; `structuredContent` is only for persistence, UI and internal logic.
- Incompatible persistence changes require an explicit format decision; do not silently reuse a version for incompatible committed formats. Validate against disposable databases, never a user's existing store.

## Documentation

- Cross-module flows live in higher-level documents; lower-level documents only describe their own inputs, outputs and error contracts.
- Read the [foundation ADR](docs/adr/001-harness-foundation.md) for branch implementation status and proposals requiring maintainer alignment. Keep accepted decisions, implemented contracts and proposals distinct. A branch implementation does not supersede an issue's agreed design, and contributors must not be required to use APIs that do not exist yet.

## Testing

- Verify that the remaining necessary changes satisfy the user flow, reusing relevant tests first; after integration, check the interfaces and flows actually affected. Reuse evidence that still applies; only expand or repeat verification for new changes, failures or unresolved doubts. Progress is not measured by test count, failure count or number of runs.
- Automated tests only protect architectural flows, public interfaces, cross-crate contracts, persistence formats, user flows and release artifacts. Assertions about private modules, helper functions, single functions, internal call order or mock configuration belong in the nearest flow test, not in standalone tests.
- Scenarios involving security, data loss, data migration, protocol compatibility or release artifacts may keep a dedicated test if they cannot be triggered reliably in a flow test.

### Verification commands

Run at the workspace root:

| Command | When |
| --- | --- |
| `./dev check` | Daily: formatting and Clippy for the CLI and App Server |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Before merging |
| `cargo test --workspace --locked` | Before merging |
| `cargo run --locked -p xtask -- check` | After changing capabilities, assembly or crate dependencies |
| `node --test scripts/*.test.mjs` | After changing development or release scripts |

The full CI sequence, including the ignored CLI profile and Node provider bridge tests, is in `.github/workflows/ci.yml`. Release preparation and draft publication follow [.agents/skills/dim-release/SKILL.md](.agents/skills/dim-release/SKILL.md).

## Architecture graphs and boundary checks

Assembly graphs and crate dependencies are computed from the actual code by `xtask`. **Do not hand-write architecture graphs, and do not edit the output under `docs/architecture/rust-agent/generated/`.**

| Command (workspace root) | Purpose |
| --- | --- |
| `cargo run --locked -p xtask -- graph` | Regenerate `docs/architecture/rust-agent/generated/`: `services.mmd` / `services.md` (service assembly), `instances.mmd` (instance keys), `modules.mmd` (module dependencies), `crates.mmd` (crate dependencies) and their `.json` snapshots |
| `cargo run --locked -p xtask -- check` | Verify crate boundaries and dependency restrictions |
| `cargo run --release --locked -p xtask -- measure` | Resource measurement; pass `--model-config PATH` (e.g. `crates/dim-agent-aimux/examples/offline.json`) |

Run `check` after adding or removing a Capability or changing assembly or crate dependencies, and `graph` after changing assembly. `check` also validates the plugin pages under `docs/architecture/rust-agent/plugins/` against the code, so update a capability's page with it; use the `.json` snapshot diff to confirm the change is as intended. Hand-written diagrams in documentation only describe the target structure; the generated output is authoritative for the current implementation.

## Agent v4 development rules

Event and state-machine principles follow the [time bus](docs/architecture/rust-agent/time-bus.md); design-time judgment follows the [design rules](docs/architecture/rust-agent/principles.md). See the [architecture overview](docs/architecture/README.md#design-documents) for the full document index.

Before designing or changing a plugin, capability or event, read [design rules §4](docs/architecture/rust-agent/principles.md) (extending behavior through the Timeline) and [ADR 002](docs/adr/002-plugin-scope.md) (scope, no wiring modules, configuration, language-neutral contracts). Every plugin design document contains a "Rule check" section that answers each of these rules (§4 rules 1–9, §1 and ADR 002 D1–D5; or states why one does not apply) and a "Requirements raised to mechanism owners" section in the format of §4 rule 8. Pull requests answer the checklist in the pull request template.

1. A new feature is a Capability (rutis `TypedPlugin` + runtime `Capability`). `Deps` declares every dependency, including those only needed for load order (`SessionGate`/`BranchGate`/`Gate`); configuration-dependent ones use `When`, and `apply` obtains dependencies from `Deps`. `apply` registers services and subscriptions and registers cleanup for acquired resources immediately with `ctx.effect`. Once `apply` completes, services and subscriptions are available; input consumption and background work start afterwards. Background tasks spawned in `apply` inject `BranchLifecycle` and wait for `activated()` (branch started and published) first; if branch assembly fails they never start. Returning from `closing()` ends them without waiting for capability unload. External crates export Capabilities; the runtime keeps no Capability enum. Capabilities are registered through Session- or Timeline-level factories that only capture configuration and instance identity. Scoped services provide and inject instance keys generated from the factory's `SessionScope`/`BranchScope` (see [single-tree assembly](docs/architecture/rust-agent/single-tree.md)).
2. Behavior that produces or consumes events with critical effects (affecting Capability execution, or context content and its processing or insertion) submits events to the Timeline and is dispatched by the Dispatcher. Capabilities coordinate through events and services declared in `Deps`; direct calls are limited to handles they own.
3. Event handlers register their own tasks and return. Network and tool waits run in the Capability's own tasks; on cancellation, release resources first, then write the terminal state. Events appended inside a handler go to the Timeline tail and are consumed later by the Dispatcher.
4. State belongs to the Capability that owns it (`StateCell`; read-only externally via `Session::get/on`). The Timeline owns persistent state and the TimelineContext owns runtime memory state: business memory state must be rebuildable from the Timeline, with only I/O resources excepted and settled to a terminal state on reload. Async operations and cancellation belong to the Capability that started them. Timeline writes go through the branch-registered `Timeline` service, and producers guarantee the validity of capability-produced input. External input enters only through `Session::input` / `Branch::input`, validated at the boundary. Ordinary callers read through `Session::history/local/trace/get/on` or `Branch` and cannot write arbitrary events.
5. Crate dependency restrictions are enforced by `xtask check`; run `check` / `graph` as described [above](#architecture-graphs-and-boundary-checks) after adding or removing a Capability or changing assembly or crate dependencies.
6. When requirements change, update or delete old assertions to match. Use `tests/integration` for everyday behavior verification. The event format fixtures in `crates/dim-agent-types/tests/fixtures/` are a frozen compatibility baseline; do not change them as a side effect of other work.
7. Before writing code, read the relevant design section (including the capability's page under [`plugins/`](docs/architecture/rust-agent/plugins/README.md)) and neighboring implementation, and reuse existing mechanisms: `Dispatcher` subscriptions, `StateCell`, and host ports (`PrepareModelHost`, `ModelHost`, `ToolHost`, etc.; see `tests/extension-a` and `tests/extension-b` for examples).
8. Settle a generation's accepted work and required terminal facts before removing its services. Detaching a client is not closing a Session. Runtime handles are not durable identities.
9. Publish Timeline events only after their atomic batch commits. Compression facts are durable history; recovery snapshots are optional derived state and cannot replace required facts.
10. Under the current public protocol, runtime context injection uses adopted inputs containing final content. Domain-specific history rendering must not introduce another context-input path. The dynamic-event projection proposal is subject to the maintainer alignment recorded in the foundation ADR.
11. A trusted in-process plugin is not sandboxed by dependency declarations. Authorization and isolation require explicit host execution boundaries. Approval and Permission govern only model tool calls; commands and endpoints run because user or project configuration enabled them (such as an MCP stdio server) are that configuration's responsibility and need no approval. See the foundation ADR's trust contract.
