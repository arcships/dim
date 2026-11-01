# AGENTS.md

This repository is Dim: a Rust coding agent (Agent v4), its App Server and the native `dimcode` CLI. See [README](README.md) and the [documentation index](docs/INDEX.md).

## Working conventions

- Set up the toolchain and workspace according to the [build environment](docs/architecture/build.md); it applies to both local development and CI.
- Write code, comments and commit messages in English. Documentation may be written in English or Chinese; translations live under `docs/zh/`, mirroring `docs/`.
- Illustrate terminal UI and interaction changes with ASCII diagrams.
- Follow the [CLI command menu contract](docs/design/cli-commands.md#command-menu) for category grouping, alphabetical ordering and keyboard-only actions in both TUI modes.
- The corresponding design document is authoritative. When a requirement conflicts with the design, update the document and get confirmation first.
- After each design or implementation, do an ablation review: check whether every change is necessary and prefer removing abstractions, wrappers, intermediate state and extra branches that do not serve the requirement. Where a simplification is plausible, try it and confirm the result still satisfies the requirement and interface contracts within the affected scope. For structure that is required, state its purpose; do not deliberately revert fixes or manufacture test failures to prove necessity. If nothing can be removed, say so instead of inventing experiments.
- Process serves delivery; keep effort proportional to the complexity and risk of the change. Handle ordinary changes directly and only delegate complex work that can progress independently. Tracking issues individually does not mean a separate agent, report or full verification run per issue.
- Development and tests use an isolated `--home` (or `DIMCODE_HOME`); never touch the product home `~/.dimcode`.
- This is a public repository. The Dim account plugin (`crates/dim-oauth`) lives here, but its OAuth client IDs and any credentials stay out of it: they are build-time secrets; see the [managed provider boundary](docs/architecture/development.md#managed-provider-boundary).
- Review correctness, security, compatibility and unnecessary abstraction before committing. Commit complete, reviewable units on their declared parent, with their documentation and observable-behavior tests. Never commit secrets or scratch experiments.

## Data and interface conventions

- Each kind of internal data has one type definition. The producer guarantees the contract; consumers use it directly without re-validating, converting or filling in fields.
- Parsing, validation and format conversion happen at the boundary where external data enters the system (user input, network responses, persisted files). Afterwards data is passed as explicit types and is not reprocessed layer by layer.
- When internal data does not match its contract, fix the producer, the type definition or the call flow. Do not mask the problem in consumers with field aliases, defaults, casts or conversion functions.
- Do not add read/parse/normalize wrappers for data of a known type; access fields and call interfaces directly. Such a function must correspond to a real external data boundary or business transformation.
- Missing values and recovery behavior must have a clear business meaning; do not introduce them because of uncertainty about what upstream returns.
- `ToolResult.content` is the only text the model can read. Handles, cursors and state the model needs later must be written into `content`; `structuredContent` is only for persistence, UI and internal logic.
- Incompatible persistence changes require an explicit format decision; do not silently reuse a version for incompatible committed formats. Validate against disposable databases, never a user's existing store.

## Comments

- Prefer code that needs no comment. A comment is only for what code cannot carry: a stable external reference (`#169`, `ADR 006 D3`), a tool or compiler directive, a `// SAFETY:` precondition, a generated-file marker, or a license header.
- Pin behavior with names, types, tests and assertion messages; put design rationale in commit messages and design documents, and external context in issues and specifications.
- Do not narrate code or tests, label sections, restate what a test asserts, or leave a TODO without an issue. Update or remove a comment when the fact it carries changes.
- `///` and `//!` document caller-visible contracts: behavior, preconditions, errors, panics, and examples where useful, not internal implementation.

## Documentation

- Cross-module flows live in higher-level documents; lower-level documents only describe their own inputs, outputs and error contracts.
- Read the [foundation ADR](docs/adr/001-harness-foundation.md) for its decision and proposals requiring maintainer alignment. Do not use it as the source of implementation status; that lives in issues. Keep accepted decisions, current contracts and proposals distinct. A branch implementation does not supersede an issue's agreed design, and contributors must not be required to use APIs that do not exist yet.
- A rule is stated once, at its layer ([ADR 006](docs/adr/006-plugin-contract.md) D1, D9): theory and contract rules do not restate a binding's spelling, and rule documents and ADRs carry no delivery status or history. Status and history live in issues.
- An accepted ADR is a snapshot ([ADR 008](docs/adr/008-documentation-ownership.md)). Do not modify or delete an accepted ADR; a proposed ADR may change until accepted. A direction change is a new ADR; a link or typo fix to an accepted ADR uses the `adr-erratum` label. Existing ADRs are not retroactively cleaned.
- A document owns only its own scope. A feature or plugin design states its own inputs, outputs, events, state, errors and declared dependencies, and links to another module's contract instead of restating or changing it. Put a cross-module requirement in the design that raises it; the owner changes its own document.
- Technical detail stays at its layer: public contract terms in the contract or design, language spelling in a binding appendix, current implementation in architecture pages. Do not describe another module's internals.
- Document type comes from the existing path; there is no manifest. Templates live in `docs/templates/`, whose README links the examples. Run `node scripts/adr-check.mjs --base <ref>` in a PR checkout; CI uses `HEAD^1`.

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

The full CI sequence, including the ignored CLI profile and Node-backed flow tests, is in `.github/workflows/ci.yml`. Release preparation and draft publication follow [.agents/skills/dim-release/SKILL.md](.agents/skills/dim-release/SKILL.md).

## Architecture graphs and boundary checks

Assembly graphs and crate dependencies are computed from the actual code by `xtask`. **Do not hand-write architecture graphs, and do not edit the output under `docs/architecture/rust-agent/generated/`.**

| Command (workspace root) | Purpose |
| --- | --- |
| `cargo run --locked -p xtask -- graph` | Regenerate `docs/architecture/rust-agent/generated/`: `services.mmd` / `services.md` (service assembly), `instances.mmd` (instance keys), `modules.mmd` (module dependencies), `crates.mmd` (crate dependencies) and their `.json` snapshots, and `catalog.md` (the service catalog and the parts that are Capabilities) |
| `cargo run --locked -p xtask -- check` | Verify crate boundaries and dependency restrictions, that `crates.*` / `modules.*` under `generated/` match the code (read-only: a stale file fails and names it; only `graph` writes them), that `catalog.md` matches the real assembly, and that `npm/branch-services.d.ts` matches the services marked `#[rutis::service]` |
| `cargo run --locked -p xtask -- types` | Regenerate `npm/branch-services.d.ts`, the TypeScript declarations of the services marked `#[rutis::service]`, after changing one of them or a type it uses |
| `cargo run --release --locked -p xtask -- measure` | Resource measurement; pass `--model-config PATH` (e.g. `crates/dim-agent-aimux/examples/offline.json`) |
| `cargo run --release --locked -p xtask -- context-replay` | Opt-in context memory baseline: 100 mock turns against a temporary SQLite store |
| `cargo run --release --locked -p xtask -- restore-replay DB SESSION` | Restore a stored session and measure restore/prepare/close memory |
| `cargo run --release --locked -p xtask -- context-breakdown DB SESSION` | Split one prepare into read/project/messages/request memory |

Run `check` after adding or removing a Capability or changing assembly or crate dependencies, and `graph` after changing assembly. `check` also validates the plugin pages under `docs/architecture/rust-agent/plugins/` against the code, so update a capability's page with it, and fails when the generated service catalog is stale; use the `.json` snapshot diff to confirm the change is as intended. Hand-written diagrams in documentation only describe the target structure; the generated output is authoritative for the current implementation.

## Agent v4 development rules

Dim's architecture and the way Dim uses rutis are documented apart ([ADR 006](docs/adr/006-plugin-contract.md) D1):

| Layer | Document |
| --- | --- |
| Dim architecture: sequence, ownership, commands, handover, `before`, rebuild, side effects, placement | [time bus](docs/architecture/rust-agent/time-bus.md), [design rules](docs/architecture/rust-agent/principles.md), [ADR 002](docs/adr/002-plugin-scope.md) |
| Dim architecture as it applies to plugins: instance tree, Dim's services and bus events, facts and their consumption, Capability, publication and closing | [plugin contract](docs/design/plugin-contract.md) |
| Using rutis: plugins as rows and parts mounted by their instance, services by name, the catalog, crossing languages, the rutis lifecycle and bus | [How Dim uses rutis](docs/design/rutis-usage.md) |
| Binding: how Rust or TypeScript writes them | The appendices of How Dim uses rutis, the [TypeScript authoring guide](docs/typescript-plugin-authoring.md), and the current Rust binding below |

A rule is stated once, at its layer. The Rust spelling of a rule is a binding, not the rule. See the [architecture overview](docs/architecture/README.md#design-documents) for the full document index.

Before designing or changing a plugin, capability or event, read [design rules §4](docs/architecture/rust-agent/principles.md) (extending behavior through the Timeline), [ADR 002](docs/adr/002-plugin-scope.md) (scope, no wiring modules, configuration, language-neutral contracts) the [plugin contract](docs/design/plugin-contract.md) (how Dim's architecture applies to plugins) and [How Dim uses rutis](docs/design/rutis-usage.md) (how parts are mounted, name services and cross languages). Every plugin design document contains a "Rule check" section that says which parts are Capabilities ([principles §1](docs/architecture/rust-agent/principles.md)) and answers each of these rules (§4 rules 1–9, §1 and ADR 002 D1–D5; or states why one does not apply), and a "Requirements raised to mechanism owners" section in the format of §4 rule 8. Pull requests answer the checklist in the pull request template.

1. A feature is split into parts by the owner of their state, and each part lives in that owner's scope: the process, a Session or a branch ([ADR 002 D1](docs/adr/002-plugin-scope.md)). A part declares every dependency, including those only needed for load order. How a part is mounted and names its dependencies is [How Dim uses rutis](docs/design/rutis-usage.md); the current Rust spelling is below. `apply` acquires resources and registers their cleanup immediately, then registers services and subscriptions. Once `apply` completes, services and subscriptions are available. Input consumption and background work start only after the Session or branch is published (`activated()`); if assembly fails, they never start. They end on `closing()` without waiting for unload, and a Session is closing from the moment its close starts.
2. Behavior that produces or consumes events with critical effects (affecting a part's execution, or context content and its processing or insertion) writes facts to the Timeline, and the Dispatcher delivers them. Parts coordinate through facts and the named services they declare; direct calls are limited to handles they own.
3. Event handlers register their own tasks and return. Network and tool waits run in the part's own tasks; on cancellation, release resources first, then write the terminal state. Events appended inside a handler go to the Timeline tail and are consumed later by the Dispatcher.
4. State belongs to the part that owns it. The Timeline owns persistent state, and business memory state must be rebuildable from the Timeline; only I/O resources are excepted, and they are settled to a terminal state on reload. Async operations and cancellation belong to the part that started them. Timeline writes go through the branch's `timeline` service, and producers guarantee the validity of what they write. External input enters only through the Session and branch input entries, validated at the boundary. Ordinary callers read through the Session and branch facades and cannot write arbitrary events.
5. Crate dependency restrictions are enforced by `xtask check`; run `check` / `graph` as described [above](#architecture-graphs-and-boundary-checks) after adding or removing a part or changing assembly or crate dependencies.
6. When requirements change, update or delete old assertions to match. Use `tests/integration` for everyday behavior verification. The event format fixtures in `crates/dim-agent-types/tests/fixtures/` are a frozen compatibility baseline; do not change them as a side effect of other work.
7. Before writing code, read the relevant design section (including the part's page under [`plugins/`](docs/architecture/rust-agent/plugins/README.md)) and neighboring implementation, and reuse existing mechanisms: Dispatcher subscriptions, owner state cells, and host ports (see `tests/extension-a` and `tests/extension-b` for examples).
8. Settle a generation's accepted work and required terminal facts before removing its services. Detaching a client is not closing a Session. Runtime handles are not durable identities.
9. Publish Timeline events only after their atomic batch commits. Compression facts are durable history; recovery snapshots are optional derived state and cannot replace required facts.
10. Events enter model context only through adopted inputs and tool results; runtime context injection uses adopted inputs containing final content. Domain-specific history rendering must not introduce another context-input path.
11. A trusted in-process plugin is not sandboxed by dependency declarations. Authorization and isolation require explicit host execution boundaries. Approval and Permission govern only actions an agent decides (tool calls). Commands that run because of configuration (MCP servers, Hook commands, workflow definitions, plugin packages) need no host-local approval and run when enabled; entries from a project source start disabled, and only the user's local configuration can enable them. File-root authorization, tool permission presets and replay never confer authority. See [ADR 003](docs/adr/003-approval-scope.md).

### Current Rust binding

Rust parts are written this way; the full binding is [How Dim uses rutis, appendix A](docs/design/rutis-usage.md#appendix-a-rust-binding). Do not use this spelling to state a rule in a design document.

- A part is a plain rutis `TypedPlugin`. `Deps` declares every dependency by name: `Keyed<T>` (read), `KeyedGate<T>` (load order only), `Option<Keyed<T>>` (read if present, without gating), `When<D>` (chosen by configuration, keys from `Names::when`), or tuples of them. Each copy's keys come from the catalog for the name and the copy's instances (`Names`: `names.part(plugin)`, `names.part_with(plugin, keys)`, and `names.key::<T>()` for a service the part provides). `apply` obtains dependencies from `Deps` and registers cleanup with `ctx.effect`.
- A Rust name's group is the one it is registered in with `names::register::<T>(catalog, group)`: `None` for the process, `Some("session")` or `Some("branch")`. A part that names a group it is not inside fails at composition.
- A feature is one process plugin that registers its Session and branch parts with the part registry (`partRegistry`, `PartRegistry::register`); they are withdrawn when it unloads. A plugin that owns nothing for the process is a `Feature` (`Feature::new(name).session(part, make).branch(part, make)`, or `.when(scope, part, generator)`), added with `AgentBuilder::feature`; one that owns process state is its own plugin, added with `AgentBuilder::plugin`. The install functions are the profile: `require_feature` and `require_part` make parts required. An App Server process row is `register_part`, and `AgentBuilder::plugin_rows` places the builder's plugins. External crates export parts; the runtime keeps no part enum.
- `AgentBuilder::session_value` provides a host value under a Session name, `AgentBuilder::process_value` under a process name.
- Background tasks wait for publication: branch parts depend on `Keyed<BranchLifecycle>` (branch started and published), Session parts on `Keyed<SessionLifecycle>` (Session published by its Agent), and both wait for `activated()`.
- Owner state is published through `StateCell` and read externally through `Session::get/on`; a branch's runtime memory state lives in its TimelineContext. External input enters through `Session::input`/`Branch::input`. Ordinary callers read through `Session::history/local/trace/get/on` or `Branch`; the facades' service reads take only `PublicService` types registered in the matching group.
- Host ports include `PrepareModelHost`, `ModelHost` and `ToolHost`; the [generated services](docs/architecture/rust-agent/generated/services.md) list them all.
