# 005: Observe runs through the Timeline and process logs

Decision status: Proposed. Supersedes the [diagnostics channel and trace archives](../architecture/rust-agent/trace.md)
and decision D9 of [idle unload](../design/appserver/idle-unload.md). The architecture pages keep describing the
implemented archive until the removal lands (D6).

## Context

Each Session has a diagnostics channel, `diagnostics/{sessionId}`. When a host enables it, the runtime emits
component and subscription lifecycle records, spans around operations, every committed Timeline event with its
before trace, and a final `Closed` record. The trace extension (`TracePlugin`) forwards the channel to a
`TraceArchive`, which writes a metadata-only (version 1) or sensitive-content (version 2) JSONL file. The App Server
exposes it as `session.create {trace}` and returns `tracePath`. A traced Session holds an idle-unload lease (D9), and
Session delete and `daemon.shutdown` wait for its archive writer.

Two needs ask for a record of what a run did: evaluation runs, which use the released packages from outside this
repository, and troubleshooting a user's Session. The archive serves neither:

1. **Business facts are a second copy.** A version 2 line's `record.payload` is the committed event and its before
   trace. Timeline storage already keeps both for every branch, with requests delta-encoded since format v17. The
   Timeline owns persistent state (AGENTS.md, Agent v4 rule 4); a parallel copy in a file adds nothing to these facts.
2. **It must be chosen when the Session is created.** A run that nobody planned to trace has no archive, and a
   restored Session is not traced (D9). The archive is complete only after the Session closes, while `dimcode exec`
   detaches without closing. No client in this repository sends `trace`, and the viewer that the `trace_archive`
   example points to (`tools/trace-viewer/rust.html`) is not in this repository.
3. **It cannot stay on.** In version 2, which the App Server writes, every `model/call` line holds the full
   request, so the file grows quadratically with the number of turns. The queue is unbounded by design (records are never dropped), so memory grows whenever the
   writer lags, and a traced Session does not unload before it closes. Business work is not blocked, but a long run is impractical to
   trace.
4. **Runtime-only records are in the wrong shape.** Spans, component and subscription records and assembly rollbacks
   help runtime troubleshooting, but they exist only per Session, only when chosen at creation, only inside a
   sensitive full-content file, in a custom format with a private subscriber per Session. Runtime troubleshooting
   needs process-level output that can be turned on at any time and that standard `tracing` tooling consumes.

Keeping it costs about 2,300 lines in files that exist only for it (runtime `diagnostics.rs`, the trace extension
and archive, the App Server `trace.rs`, `tests/integration/tests/trace.rs` and the `trace_archive` example), plus a
`Diagnostics` dependency in six capabilities and in runtime assembly, the Dispatcher, the Timeline and Session close;
a `session.create` option; an idle-unload exception; and archive draining in Session delete and shutdown.

## Decisions

### D1. Two observation layers

| Layer | Holds | Read through |
| --- | --- | --- |
| Timeline | Every committed event of every branch with its before trace and `create_at`: inputs, final model requests (`model/call`), usage including cache tokens (`model/usage`), tool calls and results, compaction, branch creation, collaboration, turn outcomes | Timeline reads (`history`, `local`, `trace`) and the Timeline export (D3) |
| Process logs | Runtime behavior: operation timing, assembly and close progress, errors | The host's `tracing` subscriber; for the App Server, `daemon.log` filtered by `RUST_LOG` |

There is no third channel. The two layers time different things:

- The Timeline gives elapsed intervals from one fact to another. It sets `create_at` when it commits an event, before
  storage publishes it, and a capability handles the event only after the Dispatcher delivers it. The interval from
  `model/call` to `step/model-output-end` therefore also covers the storage write and dispatch of `model/call` and
  ModelProvider's retries; the interval from `step/tool-call` to `step/tool-result` covers a whole batch, with its
  dispatch and Approval. Per-call `step/tool-start` and `step/tool-completed` are written only when ToolExecutor's
  `emit_progress` is set, and the default assembly does not set it.
- The process logs time an operation inside its owner, as the duration of its D4 span. `model.invoke` starts after
  `model/call` is dispatched and includes those retries; `tool.execute` includes Approval. Neither is network or
  handler time alone.

The export and evaluation designs name each interval metric by its two endpoint facts instead of treating it as
execution time. Timing adds no Timeline fact.

```
                 one run
       ┌────────────┴─────────────┐
 committed facts            runtime behavior
       │                          │
       ▼                          ▼
 Timeline storage           tracing subscriber
 (dimcode.sqlite)                 │
       │                          ▼
       ▼                    daemon.log (RUST_LOG)
 Timeline export (D3)             │
       │                          │
       └────────────┬─────────────┘
                    ▼
   evaluation · viewer · diagnosis skill
```

### D2. Remove the diagnostics channel and the trace archive

Removed:

- Runtime: `Diagnostics`, `DiagnosticRecord`, `DiagnosticEvent`, `ComponentPhase`, `SubscriptionPhase`,
  `SpanPhase`, `diagnostics_channel`, the Session and branch `Diagnostics` services, and their emission in assembly,
  the Dispatcher, the Timeline and Session close.
- Capabilities: `TracePlugin`, `TraceHost`, `trace::install`, `TraceArchive` and `ArchiveContent`; the `Diagnostics`
  dependency of ContextPreparation, ModelProvider, Prompt, ShellInput, ToolExecutor and ToolRegistry; the
  `trace_archive` example.
- App Server: the trace extension and `Traces`, `session.create {trace}`, `tracePath` in its result,
  `ServerConfig.trace_directory`, the D9 lease, and the waits for archive writers in Session delete and
  `daemon.shutdown`.
- Tests: `tests/integration/tests/trace.rs`.

Unchanged:

- Before traces in Timeline storage, and `Timeline::trace`, `Session::trace` and `Branch::trace`. They belong to the
  persistent Timeline contract.
- The `SessionExtension` mechanism, which Skills content and MCP use.
- rutis's own diagnostics snapshot, which `xtask graph` reads.
- Diagnostics that are Timeline facts, such as `CompactionDiagnosticsRecord` and `context/budget`.
- `dimcode session export`, the conversation export of `main`.

### D3. The Timeline export is the read path for complete runs

dim provides an export of one Session's whole Timeline. Its design document settles the entry point and the
fields; this decision fixes the contract:

- It reads through the store's own Timeline reader, so it follows storage format changes. Tools outside dim never
  read SQLite tables.
- It ships in the released CLI, so evaluation can use it from the published packages.
- It reads the live home or a consistent copy of the database. The database uses WAL, so copying the files of a
  running App Server is not a snapshot: copy with SQLite's backup API or after `daemon.shutdown`. It opens the
  database read-only and rejects unsupported format versions.
- It writes versioned JSONL: a header with the export version and the Session, the branch directory (parent, branch
  point, create anchor), then every event of every branch in branch-local sequence with its before trace. Events and
  before traces use their existing serialization; there is no second business schema.
- It contains only that Session's Timeline and never reads provider, credential or settings tables; provider
  records can hold inline API keys. The output is still sensitive, because it holds prompts, code and tool output.

Evaluation, a viewer or a diagnosis skill reads the export and `daemon.log`; none of them defines a format of its own.

### D4. Runtime operations become `tracing` spans

The operations that the channel spanned keep a span in the process subscriber: `prompt.load`, `context.prepare`,
`model.invoke`, `tool.attach`, `tool.execute`, `shell.run`, `timeline.close` and `session.close`. A span carries the
Session and branch ids and the operation's own ids (`turn_id`, `request_id`, `call_id`, `run_id`); `prompt.load`
keeps its source and scope. Spans use the process's `tracing` dispatcher; there is no dispatcher per Session. The
App Server's subscriber reports span close with its duration, so `RUST_LOG` shows operation timing.

Span names and fields are log output, not a compatibility contract, and no test asserts them. Component and
subscription lifecycle records are not replaced: a failed assembly is returned to the caller as
`session_build_failed`, and `xtask graph` shows the assembled services.

### D5. Compatibility

- `session.create` no longer accepts `trace`. Its parameters reject unknown fields, so a client that still sends
  it receives `invalid_params`. No client in this repository sends it. The result no longer has `tracePath`.
- Existing archive files stay where they are; no reader ships for them.
- The database format does not change; the archive never wrote to it.

### D6. Order

1. This decision.
2. The removal (D2, D4, D5) with its documentation: delete [trace](../architecture/rust-agent/trace.md) and its
   index entry; update idle unload (D9, the lease table, the member list, §13 acceptance item 11), Session deletion
   and fork (the archive wait, the trace archive note, the fork parameters row), single-tree (the `trace` instanced
   group, `AgentHost`, the shutdown drain, the compatibility row), prompt (the Trace row, the diagnostics paragraph,
   verification), event-driven flows and interrupt (facts "kept in the Timeline and trace"), ShellInput A11,
   ContextCompaction, auxiliary model item 6, the Trace rows of the design overview, request-delta storage (its
   citation of trace.md for the final request), the App Server README (shutdown and the test home), their `docs/zh`
   translations, and the generated graphs.
3. The Timeline export (D3): design document, then implementation. Evaluation depends on this step, not on step 2.

Step 2 does not wait for step 3. No client in this repository enables the archive, but the public
`session.create {trace}` parameter lets an external RPC client enable it today, and step 2 breaks such a client as D5
declares. Waiting would not avoid that break: the export replaces the archive's read path, not the RPC option.

## Consequences

- Evaluation runs each case with an isolated `--home` and reads the Timeline export of its Sessions: usage and cache
  metrics from `model/usage`, and named fact-to-fact intervals (D1) from `create_at`.
- Troubleshooting a user's Session uses that Session's export plus `daemon.log`. The user reviews both before
  sending them.
- No longer recorded anywhere: per-Session component and subscription lifecycle, and the rollback records of a
  Session whose assembly failed. Only the trace tests consumed them.
- Neither layer records time to first token (model progress is not durable) or process resources, which
  `xtask measure` measures. Evaluation decides separately whether it needs them.
