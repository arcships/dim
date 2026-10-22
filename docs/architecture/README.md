# Architecture

```text
Terminal / npm launcher
          |
     Rust dimcode
          | authenticated local NDJSON RPC
  Rust dim-app-server
     |            |
  Agent v4     Provider host
     |            |-- API-key/native model connections
  Timeline        |-- optional managedProvider service
  Dispatcher              |
  Capabilities       rutis Node bridge
  Storage                 |
                  separately supplied account plugin
```

`dim-agent-types` owns serialized data. `dim-agent-runtime` owns instance construction, timeline ordering and close coordination. Capabilities own their work, cancellation and state. Model and tool adapters provide typed ports; transport and product configuration belong to the App Server. The CLI is a client, not another agent execution engine.

Services use rutis 0.10.0 TypedPlugin/Capability contracts. Dependencies are declared once; each instance scope uses the appropriate keys. Managed JS services are optional gated bindings on the same rutis tree. The process owns the Node runtime and drains its work at close. If an account plugin is unavailable, requests for its managed providers fail explicitly while API-key providers remain available.

Capability factories are pure and preserve plugin names and dependency keys. Configuration that must survive reload belongs to the captured plugin configuration; each `apply` acquires fresh I/O resources and registers their cleanup. FileStorage clones share the append-only authorized-root identities, while directory handles, operations and leases belong to one service generation. Rebuilding from the initial root list would let a later project reuse an earlier project's index.

Workspace tool hosts register `FileWriteCoordinatorPlugin` at process scope and clone it across reloads. `write` and `edit` share per-path admission across Sessions, retain their lock through blocking I/O and binding publication, and stage complete bytes before atomic replacement. Existing revision, identity and metadata are checked immediately before commit; new targets use create-if-absent publication. This is not compare-and-swap against unrelated processes or parent-directory replacement. Native metadata is preserved (macOS ACL/xattrs/resource forks, Linux xattrs/ACL, Windows DACL). Unchanged Linux security labels are retained without rewriting them. Windows alternate streams or ownership/security that cannot be preserved fail unchanged. An error after installation reports that the target may already have changed. Re-read before retrying.

Hosts register process modules before awaiting `AgentBuilder::install`. Scoped module names are allocated against rutis `Builtins` with `#n` suffixes on collision; the assembly only keeps reserved identities, not a second module registry. Hosts also pass identities absent from `Builtins` to `install`: external module names and row IDs (both the managed JS entry path and its row ID, for example). Extension group IDs are public instance selectors and cannot be silently renamed: collisions are rejected before loading.

One startup deadline covers loader readiness, reconcile and process activation. Scoped dependencies use `InSession`, `InBranch`, gates and `Contributions`; the unused `Named` wrapper is not part of the API.

Session and child-branch opening also have a single startup budget. Opening handlers register `BranchOpening` receipts and return without awaiting host I/O on the Dispatcher. Each capability owns and joins its work; the tree waits for the receipts and their committed dispatch fence before publishing readiness or the first child input. Closing signals initial prompt loading before dispatch drains. Deadlines request cancellation, not abandonment: a non-cooperative host still has to settle before cleanup completes.

The public host exposes provider/account operations and leases request-local credentials. It does not implement a particular account realm, PKCE flow, token exchange, refresh policy, private catalog projection or provider-specific error classification. Those behaviors belong to the separately supplied plugin. The trusted native host owns generic `pluginFiles` transactions: credential-file bytes, including refresh tokens, cross the local Rutis bridge for storage, without being interpreted by the host. This is source separation, not a sandbox against the host or same-user processes. Refresh tokens are excluded from provider connection DTOs, model requests, terminal RPC, history and logs. Terminal/history projection also excludes access credentials, authorization codes and PKCE state. A credential or recovery result acquires its release guard inside the accepted, generation-tracked operation, before validation or handoff to a caller; a dropped waiter cannot orphan it. A delivered or buffered lease retains a tracker token until release is enqueued, so generation cleanup cannot outrun its owner. Release uses that generation's dispatch, not whichever plugin is currently installed.

Transport admission is bounded per connection: 16 MiB per decoded frame (excluding NDJSON delimiters), 64 ordinary in-flight requests plus 8 reserved control slots, and 1024 outbound frames with a 32 MiB encoded-byte budget held through writing. Saturation closes a slow output consumer rather than dropping durable events on a healthy connection. Call progress is best effort: it has its own allowance of 256 frames and 4 MiB in the same ordered queue and is dropped, never closing the connection, when it does not fit. Clients reconnect and use persisted history to resume. Product model selection uses explicit request, Agent profile, then product defaults; collaboration members use the same declared model-selection host to resolve their capability snapshot.

Timeline and Dispatcher deliver both business events and interrupts. Capabilities cancel their own work and emit terminal outcomes after cleanup. A stopped turn must settle before the CLI accepts another prompt. Client detach does not delete or close a persisted session. Storage rejects incompatible data explicitly; see the [storage format](../design/appserver/native-storage-format.md).

The CLI disables terminal echo before inviting callback input and restores it on every exit. Built-in shell approvals intentionally show the command and cwd for informed consent; commands may themselves contain sensitive values. Arbitrary extension arguments are not printed. The npm launcher requires Node 22.19+ on the 22.x line or 23.11+ so POSIX execution can replace the wrapper without duplicate terminal signals.

Model adapters validate incoming HTTP/SSE data at their boundary. Event serialization is covered by frozen compatibility fixtures; the original TypeScript source generator is not needed at build time. `ProviderPresentation` transports generic nonempty source/reason/action strings and public diagnostics; private classification policies are not part of the Rust contract.

AiMux text and reasoning IDs identify active segments: a completed ID may start a new segment, while overlapping starts, unpaired deltas/ends and unfinished successful output remain errors. Each lifetime keeps its own content and continuation metadata. Tool-call identity and native provider metadata follow their separate contracts; neither is rewritten to accommodate text/reasoning ID reuse.

## Crates

| Crate | Contents | Dependency limits |
| --- | --- | --- |
| `dim-agent-types` | Events, messages, model requests, tool results, identities | No Tokio, rutis, HTTP or capabilities |
| `dim-agent-runtime` | Agent/Session, Timeline, Dispatcher, TimelineContext, the `Branch` facade, capability registration and control ports | Does not reference default capabilities or the App Server |
| `dim-agent-capabilities` | Default capabilities and their host ports | Does not depend on the App Server |
| `dim-agent-aimux` | Model configuration, model table, rutis plugin, request and stream mapping | aimux types stay inside the crate; no App Server |
| `dim-app-server` | RPC, connections, Session registry, approval host, transport, daemon | Composes runtime and capabilities; does not reference the CLI |
| `dim-cli` | Terminal client and local RPC transport | Depends only on `dim-agent-types`; not an execution engine |
| `xtask` | Graph generation, boundary checks, measurement | Development only |

The runtime defines control interfaces and factory contracts; host ports for models, tools and context are defined by the owning capability crate. External capability crates depend on the runtime, never the reverse.

## Objects

| Object | State and resources | Boundary |
| --- | --- | --- |
| Agent | Configuration, Session registry, capability factories | Does not run the model loop; one configuration can create many Sessions |
| Session | Workspace, approvals, TimelineTree, Session-scoped capabilities | Independent of connections; Sessions share no mutable business state |
| Timeline | Hot window, publish watermark, ancestor chain; events live in the TimelineStore | The single historical fact; published events never change |
| TimelineContext | Runtime owner of one loaded branch: its subtree, Timeline, Dispatcher and `BranchLifecycle` | A container only: business state belongs to capabilities and is rebuilt from the Timeline |
| Branch | Facade for validated input, stop, idle, configuration, read-only history and capability state | Callers cannot write arbitrary events |
| Capability | Own state, declared dependencies, subscriptions, resources and errors | Defined in any crate; the runtime does not enumerate them |
| Connection | Pending requests, subscription cursors, I/O tasks, outbound queue | Disconnect releases only connection resources |

Each process has one rutis root; Sessions and branches are scoped subtrees keyed by instance. The Timeline owns persistent state and the TimelineContext owns runtime memory state: only I/O resources (processes, connections, streams) cannot be rebuilt, and they settle to a terminal state on reload or restart.

## Event conventions

- Only Timeline commits assign sequence numbers; ordering is by sequence, never by completion time.
- Each branch has one serial Dispatcher. Appends made inside a handler go to the tail and are dispatched later, not recursively.
- Handlers register owned work and return; network and tool waits never occupy the Dispatcher. Business failures are written as events.
- A branch is running while the Loop has an open turn (`turn/start` dispatched, `turn/end` not yet); stopping is running plus interrupted.
- InputQueue decides when input is adopted (`input/applied`); Loop decides when it is consumed (`input/consumed`).
- `stop/interrupt` is a signal: each capability cancels its in-flight work, releases resources and then writes its terminal outcome.

## Design documents

Detailed contracts live under [`rust-agent/`](rust-agent/) and are written in Chinese:

| Document | Topic |
| --- | --- |
| [principles](rust-agent/principles.md) | Design rules: bounded, orthogonal capabilities and ownership |
| [time-bus](rust-agent/time-bus.md) | Event and state-machine axioms (draft) |
| [single-tree](rust-agent/single-tree.md) | rutis tree assembly, keys, visibility, startup, close and reload |
| [lifecycle](rust-agent/lifecycle.md) | Scopes, cancellation, settlement and resource cleanup |
| [interrupt](rust-agent/interrupt.md) | Stop and interrupt semantics |
| [event-driven-flows](rust-agent/event-driven-flows.md) | Approval, detached work, branch creation, compaction, broadcast |
| [timeline-storage](rust-agent/timeline-storage.md) | Timeline write, publish and read contract |
| [tool-contributions](rust-agent/tool-contributions.md) | Tool contributions, registry and binding |
| [prompt](rust-agent/prompt.md) | Prompt lifecycle and configuration adoption |
| [plugins](rust-agent/plugins/README.md) | Per-capability dependencies, events and state, checked by `xtask check` |

## Generated architecture

```sh
cargo run --locked -p xtask -- graph
cargo run --locked -p xtask -- check
```

Graphs are generated from actual runtime bindings and Cargo/syn analysis into `docs/architecture/rust-agent/generated/`. Do not edit generated graphs by hand. `tests/extension-a`, `tests/extension-b` and `tests/integration` verify that independent external capabilities can participate without depending on internal implementation details.

## Daemon profile identity

`hello` declares a product, configured or mock daemon profile. Configured identity includes the SHA-256 of the exact validated model configuration bytes, without credential values or raw configuration. The CLI requires the expected profile and matching configuration digest before any session/account operation. A missing or incompatible identity is rejected without deleting its endpoint or stopping that process. Using separate homes for different configurations remains recommended.
