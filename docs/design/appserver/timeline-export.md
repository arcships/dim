# Timeline export

Status: Implemented in [#275](https://github.com/arcships/dim/pull/275), whose first commit was this design for
review. Decision: [ADR 005](../../adr/005-observability.md) D3, step 3
of D6 ([#274](https://github.com/arcships/dim/issues/274)). Scope: a read-only reader in the App Server's storage
module, run as a one-shot mode of the `dim-app-server` binary, and the CLI command `dimcode export`. No plugin, no
RPC method, no Timeline fact.

## 1. Summary

`dimcode export SESSION_ID` writes one Session's whole Timeline as versioned JSONL: a header, the branch directory,
then every event of every branch in branch-local sequence with its before trace, and an end line. It is the read path
for complete runs (ADR 005 D1): evaluation and troubleshooting read it together with `daemon.log`, and neither defines
a format of its own.

- **Entry point.** The CLI runs its sibling `dim-app-server` binary in a one-shot export mode. The reader is the
  store's own Timeline reader, in the same build as the daemon that wrote the store, so it follows storage format
  changes (§2).
- **Source.** The live home (`<home>/rust/dimcode.sqlite`) or a consistent copy given with `--database`. The database
  is opened read-only, without the daemon's lease and without an upgrade; the whole read is one SQLite read
  transaction, so an export of a live home is a snapshot (§4).
- **Content.** Only that Session's Timeline, in the existing serializations of events, before traces and branch
  information. The reader never reads provider, credential or settings tables (§6).
- **Sensitivity.** The output holds prompts, code and tool output. The header says so, a file is created private and
  never overwritten, and the help text and README warn (§7).

Not part of this design: the request snapshot filters of the feature list (reasoning, tool records, compacted
content), a viewer, an evaluation harness, and reading trace archive files written before ADR 005.

## 2. Entry point

| Option | Reads through the store's reader | Ships in the released CLI | Live home or copy, read-only | Verdict |
| --- | --- | --- | --- | --- |
| A. `dimcode export` runs the sibling `dim-app-server --export-timeline` | Yes: the storage module's reader, same build as the daemon | Yes: both binaries are in every platform package (`scripts/pack-release.mjs` asserts it) | Yes: a one-shot process opens any path read-only; no daemon involved | **Chosen** |
| B. `dim-cli` links `dim-app-server` and opens the store itself | Yes | Yes | Yes | Rejected: the CLI becomes a second store owner and links the whole App Server (runtime, plugins, SQLite) into a client that today only speaks the protocol |
| C. A daemon method, such as `session.timeline.export` | Yes | Yes | No: the daemon opens its database read-write under its lease and upgrades older formats, so it cannot read a copy read-only; it needs a running daemon on that home | Rejected: also adds a protocol surface that must page large exports under the 16 MiB frame limit |

A follows the existing pattern of `dim-app-server --connect-pipe`, a one-shot client mode of the same binary that
loads no product state. The CLI already finds the sibling binary, and `--server PATH` overrides it, so the CLI and
the reader always come from one build. `dimcode export` is the command name that
[CLI commands](../cli-commands.md) reserved for the request snapshot export. Every final model request is a
`model/call` fact (ADR 005 D1), so this export carries those snapshots; the snapshot filters stay out of scope.

`dimcode session export` stays what it is: a conversation export of `main` through the running daemon's
`session.history`, in Markdown or JSON.

## 3. Structure

```
 dimcode export S [--output F] [--database D] [--home H]
   │  D defaults to H/rust/dimcode.sqlite (H: --home, DIMCODE_HOME, ~/.dimcode)
   │  opens F create-new and private, or keeps its own stdout
   │  starts no daemon and opens no socket
   ▼
 dim-app-server --export-timeline S --database D      one process, no plugins
   │
   ▼
 SqliteStorage::open_read_only(D)
   │  read-only connection, no lease, BEGIN: one snapshot for the whole read
   │  exact schema check: the current format or one this build upgrades from
   ▼
 storage::export
   │  validate_session(S): the branch directory, validated as a restore validates it
   │  read_range per branch, 1..=head, in pages: events with before traces
   ▼
 stdout: header, branch lines, event lines, end line   ──►  F or the CLI's stdout
```

The export lives in the storage module beside `timeline.rs`, so it uses the same row decoding, request content
reconstruction and validation as Session restore. It adds one constructor to `SqliteStorage` and one mode to the
binary's `main`; the daemon's paths do not change.

## 4. Opening the database

1. **No store is created.** The path must name an existing regular file. The export creates no directory, no `.lock`
   file and no database, and runs no schema statement.
2. **No lease.** The daemon's admission lease (`<database>.lock`) excludes a second writer. The export is not a
   writer, so it neither takes nor waits for the lease, and it runs while a daemon owns the database.
3. **Read-only connection.** `SQLITE_OPEN_READ_ONLY`, with the daemon's 5-second busy timeout, the setting
   `PRAGMA busy_timeout` controls; it applies to this connection only. None of the daemon's other pragmas
   (`journal_mode`, `synchronous`, `foreign_keys`, `fullfsync`) is set. The database file is never written. As for
   any SQLite reader of a WAL database, the `-wal` and `-shm` files beside it are used, and created empty when they
   are absent (after a clean shutdown or for a fresh copy); SQLite cannot read a WAL database whose directory is
   read-only and lacks them.
4. **One snapshot.** The connection runs `BEGIN` and reads everything; closing it ends the read transaction. The first read fixes the snapshot:
   commits the daemon makes during the export are not seen, so all branches, heads and events come from one point.
   Readers do not block the daemon's writes; the WAL cannot be checkpointed past the snapshot until the export ends.
5. **Format.** Inside the snapshot, the schema must equal exactly one this build opens: the current format, or one an
   upgrade step starts from (v13, v15, v16, v17 and v18 for v19), with a matching marker. Upgrades only add tables and
   replace the marker and never rewrite a row, so the reader that a daemon uses after an upgrade reads these stores
   unchanged; the export reads them as they are and does not upgrade them. Any other schema or marker (a newer
   dimcode's store, a changed schema, the legacy TypeScript store) fails with `unsupported Timeline storage format`,
   and the database is left as it was.

```
 daemon (writer)                    export (reader)
   commit n                           BEGIN
   commit n+1  ──────────────────►    first read: snapshot holds commits ≤ n+1
   commit n+2                         reads branches and events (never sees n+2)
                                      end line, close
```

**Copies.** The live home needs no copy. To export elsewhere or later, copy with SQLite's backup API (for example
`sqlite3 dimcode.sqlite ".backup copy.sqlite"`) while the daemon runs, or copy `dimcode.sqlite` together with any
`dimcode.sqlite-wal` beside it after `dimcode daemon stop` (`daemon.shutdown`). A clean shutdown normally checkpoints
the WAL into the database and removes it, but not while another connection, such as a running export, still has the
database open. Copying the files of a running daemon is not a snapshot (ADR 005 D3). Then pass the copy with
`--database`.

## 5. Output

UTF-8 JSONL, one object per line, each with a `kind`. Envelope fields are snake_case, like the native serializations
they carry. Export version 1:

| Line | Fields |
| --- | --- |
| `header`, first | `export` (1), `session_id`, `store_format` (the format of the database read), `sensitive` (always `true`) |
| `branch`, one per branch | `info`: the branch's `TimelineInfo` as stored (`id`, and for a child `parent`, `branch_point`, `create_anchor`); `head`: the branch's last sequence in the snapshot |
| `event`, every event | `branch_id`; `event`: the `AgentEvent` serialization; `trace`: the before trace (`TimelineTrace`) or `null`; `batch_end`: the last sequence of the atomic batch the event was committed in |
| `end`, last | `branches` and `events`: the counts written |

```
{"kind":"header","export":1,"session_id":"S","store_format":18,"sensitive":true}
{"kind":"branch","info":{"id":"main"},"head":41}
{"kind":"branch","info":{"id":"B","parent":"main","branch_point":12,"create_anchor":13},"head":7}
{"kind":"event","branch_id":"main","event":{"id":"…","sequence":1,"create_at":1761000000000,"type":"input","name":"message","data":{…}},"trace":null,"batch_end":1}
…
{"kind":"event","branch_id":"B","event":{"id":"…","sequence":7,…},"trace":{"event_id":"…","original":{…},"steps":[…]},"batch_end":7}
{"kind":"end","branches":2,"events":48}
```

- **Order.** Branch lines, then event lines, both in the store's directory order (by branch id). Each branch's events
  run from 1 to its `head`: its own Timeline, without the parent prefix it inherits; a reader rebuilds the tree from
  `parent` and `branch_point`.
- **Every event.** Control events (`stop/*`, `input/consumed`), extension events, `model/call` with its full final
  request, usage, compaction, branch creation and failure, collaboration and turn outcomes. Nothing is filtered,
  merged or re-derived; output deltas appear as the separate events they are.
- **No second schema.** `event`, `trace` and `info` are the existing serde serializations of `AgentEvent`,
  `TimelineTrace` and `TimelineInfo`. Event bodies change with their event types, not with the export version.
- **Versioning.** A reader ignores unknown line kinds and unknown envelope fields. Adding either keeps the version;
  removing or changing the meaning of one raises it.
- **Completeness.** Only a file that ends with the `end` line, whose counts match, is complete.

## 6. What the export reads

| Table | Read | Why |
| --- | --- | --- |
| `agent_store_format`, `sqlite_master` | Yes | Format check (§4) |
| `agent_sessions` | The Session's row | The Session exists and its row parses, as restore checks; the configuration is not written |
| `timeline_branches`, `timeline_events`, `timeline_content` | That Session's rows | The Timeline |
| `providers`, `provider_imports`, `provider_selections`, `settings_documents`, `agents`, `session_owners`, `session_metadata`, Session files, every other table | Never | Provider records can hold inline API keys; settings and other process state are not the Timeline |

The header carries no Session configuration: the model of each call is in its `model/call` request, and D3 limits
the export to the Timeline.

## 7. Sensitivity and output handling

- The header's `sensitive: true` travels with the content.
- With `--output PATH`, the CLI creates the file with create-new semantics and mode 0600 on Unix, as
  `dimcode session export` does, and never overwrites. Relative paths use the CLI's current directory. On success it
  prints `Exported Timeline (sensitive): PATH` to stderr. If the export fails, the CLI removes the file it created.
- Without `--output`, the JSONL goes to stdout; the user chooses where it lands.
- The help text and the CLI README state that the export holds prompts, code and tool output, and that API keys are
  not read but may appear in the Timeline if a user or tool wrote them there.

## 8. Interval metrics

The export adds no timing field. Every event carries `create_at`, the time the Timeline committed it, before storage
published it. A consumer names an interval by its two endpoint facts and does not call it execution time (ADR 005 D1):
for example, `model/call → step/model-output-end` also covers the storage write, dispatch and ModelProvider's
retries, and `step/tool-call → step/tool-result` covers a whole tool batch with its Approval. Operation timing inside
an owner is in `daemon.log` as the D4 spans.

## 9. Errors

| Case | Result |
| --- | --- |
| No database at the path | `No dimcode database at PATH`; exit 1 |
| Unsupported format | `unsupported Timeline storage format at PATH; the database was not changed`; exit 1 |
| No such Session | `Session S is not in PATH`; exit 1 |
| The stored Timeline fails validation | The reader's error, as a restore would report it; exit 1 |
| A read-only directory without the `-wal` and `-shm` files | SQLite's error; copy the database to a writable directory; exit 1 |
| Write failure (disk full, closed pipe) | Exit 1; a partial file is removed |
| `--output` names an existing path | Refused before the export starts; exit 1 |

Exit 1 is the CLI's existing code for configuration and transport errors. The export never retries.

## 10. Changes to other documents

- [CLI commands](../cli-commands.md): `dimcode export` is this export (it was reserved for the request snapshot
  export); the "Other target commands" paragraph changes accordingly.
- [CLI README](../../../crates/dim-cli/README.md): the command, its options and the sensitivity note.
- [App Server README](../../../crates/dim-app-server/README.md): the `--export-timeline` mode.
- [ADR 005](../../adr/005-observability.md) and the [ADR index](../../adr/README.md): step 3 implemented.
- [Feature list](../../feature-list.md): `dimcode export` exports a Session's Timeline.
- [Documentation index](../../INDEX.md): this page.

## 11. Rule check

| Rule | How this design complies |
| --- | --- |
| [Principles](../../architecture/rust-agent/principles.md) §1 (plugins and Capabilities are bounded) | Not applicable: no plugin or Capability is added |
| §4 rule 1 (commands for others' state) | Not applicable: the export changes no state |
| §4 rule 2 (handover) | Not applicable: no handover |
| §4 rule 3 (before callbacks) | Not applicable: none added; existing before traces are read as stored |
| §4 rule 4 (no business meaning in core) | The export copies events and traces without interpreting them |
| §4 rule 5 (batches) | Not applicable: no write; `batch_end` reports the stored batch boundaries |
| §4 rule 6 (model context) | No model-visible effect |
| §4 rule 7 (public events and services) | The CLI uses the binary's documented mode; the export reads only the store's own module |
| §4 rule 8 (infrastructure unchanged) | Timeline, Dispatcher, event envelope, storage format, assembly and the Session facade are unchanged. The reader is SessionStore's own, D3 decides the export, and the only addition is a read-only open of the existing store |
| §4 rule 9 (only decision facts in the Timeline) | No fact is added; timing stays out of the Timeline (§8) |
| AGENTS.md v4 rule 4 (ordinary callers read through `Session` or `Branch`) | The export is not a caller inside a running Agent; it is the store's reader, which D3 names as the read path, and nothing outside dim reads the tables |
| [ADR 002](../../adr/002-plugin-scope.md) D1 (scope follows state) | No plugin: the export runs outside the plugin graph, like `--connect-pipe` |
| ADR 002 D2 to D5 (no wiring modules, dependency direction, configuration, language-neutral contracts) | No assembly and no configuration is added; the reader stays in the App Server's storage module and uses the runtime's types; the JSONL lines are a language-neutral contract defined here (§5) |
| ADR 005 D3 | §2 (store's reader, released CLI), §4 (live home or copy, read-only, formats), §5 (versioned JSONL, existing serializations), §6 (only the Timeline), §7 (sensitive), §8 (interval names) |

## 12. Requirements raised to mechanism owners

None. Storage gains a read-only open inside its own module; no other owner changes.

## 13. Acceptance (flow tests)

Disposable databases and an isolated home only, never `~/.dimcode`; no timing thresholds.

1. **Multi-branch Session with compaction and tool calls.** A Session with tool calls, a completed compaction and a
   child branch, written by a real App Server. The export has one header, one branch line per stored branch whose
   `info` equals the stored directory, and for each branch exactly its events from 1 to `head`, equal to the
   TimelineStore's reads of that branch (`read_segments`, `trace`) with their `batch_end`; the end counts match.
   `model/call`, tool call and result, compaction and `branch/create` facts are present.
2. **Read-only open.** An export of a live home succeeds while the daemon runs and owns the lease. After shutdown, an
   export leaves the database file byte-identical, and an export of a file the user cannot write succeeds.
3. **Unsupported format rejected.** A disposable database with a newer marker, and one with a changed schema, fail
   with the unsupported-format error and stay byte-identical. Stores of formats this build upgrades from, a v13 schema
   without the later tables and a v17 marker, are read without upgrade.
4. **Consistent copy.** A copy made with SQLite's backup API while the daemon runs exports the same lines as the live
   home at the same point.
5. **No settings or credential data.** A provider record with an inline API key and a settings document, each holding
   a unique marker, are stored before the run; neither marker appears in the export.
6. **CLI.** `dimcode export` with an isolated `--home` and the built server writes the same lines to `--output` and
   to stdout, refuses an existing output path, removes its file when the export fails, and starts no daemon.

No product plugin registers a `before` transform, so a test plugin's transform supplies the before traces of item 1.
Flow tests: `timeline_export_reads_a_live_product_home_and_its_copies` and `timeline_export_carries_before_traces`
in the App Server's `transport_flow`, and `export_writes_the_timeline_without_starting_a_daemon` in the CLI's
`cli_flow` (run by CI with the built App Server).

## 14. Decisions

| # | Decision | Alternatives | Reason |
| --- | --- | --- | --- |
| D1 | `dimcode export` runs the sibling binary's one-shot mode | CLI links the store; daemon method | §2 |
| D2 | One read transaction for the whole export | Per-read autocommit as the daemon reads | A live home export must be one point in time; per-read snapshots could mix heads and events |
| D3 | Read the current format and the formats this build upgrades from, without upgrading | Current format only | Upgrades never rewrite rows, so the daemon's reader already reads them; refusing would force a user to start a daemon, which upgrades the store, just to export it |
| D4 | One line per branch and per event, with an end line | One JSON document; no end line | Large Sessions stream without holding everything; the end line detects a truncated file |
| D5 | Snake_case envelope | camelCase as in the protocol | The carried serializations (`create_at`, `branch_point`, `event_id`) are snake_case; one casing per file |
| D6 | No Session configuration in the header | Configuration in the header | D3 limits the export to the Timeline; the model of each call is in its request |
