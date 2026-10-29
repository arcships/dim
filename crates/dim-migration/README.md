# Migration

`MigrationPlugin` is an on-demand process plugin. It owns legacy decoding,
conversion, native data writes, task progress, results and retry receipts. Clients
mount it for an invocation and unload it after settlement. Other plugins and the
App Server read ordinary native data; they have no migration-specific API or path.
Desktop can use the same service without copying its conversion code.

## Host binding

Mount `MigrationPlugin::new(product_home, native_directory, theme_ids)`, await the
plugin and get `Migration`. The product home exists; `native_directory` is relative
to it. The CLI passes `rust`, matching its normal native profile. The plugin does
not select Global/CN directories or invent automatic triggers.

- `inspect(source, stopped)` returns an inventory.
- `run(source, stopped, retry_of)` returns a durable report; `progress()` subscribes
  to `Progress { task, recent }`, and `cancel_current()` requests cancellation.
  `recent` holds at most eight results, appended only after their durable write.
- `list()`, `report(task)` and `cancel(task)` work without a daemon.

Close legacy writers and target clients first. The CLI uses normal verified daemon
shutdown before run/retry. The plugin takes the native store's physical lease and
releases it after its connection closes. Queries and inspection do not open the
native database. Unload rejects new work, cancels pending work, drains accepted
commits and persists the terminal result before releasing its FileStorage child.

## Native adapter

The adapter targets native format 22, initialized and checked by the existing
`SqliteStorage` API. It never sends source schemas to business owners. It rejects
unsupported formats, including future format 23, without downgrading or removing them.

Sessions use existing `SessionConfig`, ownership and branch records. The adapter
writes canonical event/output row encodings and complete message batches. Imported
user inputs are adopted and consumed; assistant content and tool results use the
existing content records. A `migration/imported` extension and source tags identify
the import, original message IDs and timestamps. There are no historical model
calls, model starts, tool dispatches, approvals or usage measurements.

A deterministic `migration` context replacement preserves the legacy effective
summary and suffix, with an explicit source message. It performs no model call.
Its diagnostics explicitly mark token estimates unavailable. Full original content
remains in history. Encrypted remote-compaction Sessions are deferred as a whole;
they never fall back to all original text. Archived Sessions are also deferred.

Files are staged under the ordinary database session-files directory with immutable
permissions. One transaction publishes the complete Session, history, ownership,
metadata, file ownership and receipt. Failed preparation discards this import's
unowned files. Ordinary deletion and fork retain their existing file behavior.
`legacy-history.json` preserves source metadata, relations, attachment/blob paths,
queued inputs and historical results; available artifacts are linked without
resuming their execution or authority.

Provider records, selections and user settings use their existing tables and
formats. Migration-private receipts and field coverage are entries under the
`migration/` namespace in the existing generic document table. No table, native
version, protocol method or other module is extended. Receipts survive normal
business deletion. Account files retain private receipts alongside their normal
fields; unrelated accounts and target state are preserved.

Provider conversion covers static credentials, model sources and overrides,
preferences, network settings, enabled state and scoped selections. Unsupported
settings are reported without sealing them as imported. Existing configured data
wins. Compatible account records retain ordinary expiry and recovery behavior;
MCP credentials are bound to the configured server URL, resource and client.

Dim token/state pairs are validated as one account record before publication,
including explicit signed-out state without tokens. Invalid state retains that
account at source and leaves unrelated target fields intact. The integration flow
loads the result through the unmodified DimOAuth provider's public status path.

Local summaries retain their compatible suffix before a revert. A summary or
cursor that crosses the revert boundary defers the Session instead of restoring
the compressed original text into its effective context.

Shared settings choose Desktop values before comparing with the target. Settings
and receipts commit together. Client preference files use atomic replacement and
an intent record; an ambiguous interrupted replacement is a conflict, preventing
replay over a later clear. Explicit shell auto-detection is a configured choice.
Skills IDs and command permission groups require exact compatible semantics. Web
imports explicit Brave settings and its enabled choice; unsupported adapters stay
at source. Calendar-compatible schedules retain their enabled state and map bound
Sessions through successful receipts.

Source JSON is limited to 8 MiB, copied blobs and archives to 16 MiB, and each
converted history record to 8 MiB. Invalid or oversized objects fail before Session
publication. Images that fit the message and effective-context record budgets stay
inline; larger images remain complete managed Session files with readable path
references and explicit inline-image omissions. Oversized original text is rejected
without truncation. Source databases and committed WAL are copied through FileStorage;
only the copy is opened by SQLite. Source files and authorized paths remain intact.

## Tasks and verification

`migration/tasks/<uuid>/state.json` and per-object results are private format 1.
Reports hold identities and fixed diagnostics, never tokens or conversation bodies.
A run lease distinguishes live work from interruption. Retry creates a new linked
task and reconciles native receipts; deferred objects remain eligible for later
import. Cancellation waits for accepted work and does not roll back committed data.

## Terminal binding

The CLI creates `terminal::Terminal::new(json)` once and calls its
`run(&migration, source, stopped, retry)` method. It delegates cancellation and all
progress rendering to that method. `is_interactive()` selects compact human output;
other modes retain the CLI's full JSON result. Batch errors use `Terminal::error`.

The renderer owns its alternate screen, restores the screen and cursor on exit or
drop, and refreshes at most ten times per second. It displays the current step,
step count, elapsed time and the recent bounded result snapshot, without rereading
the ledger. A small terminal shows five rows or a compact display. Unknown totals
use an activity indicator. Completion leaves a summary and a report command;
non-interactive runs emit only stage changes to stderr, and `--json` stays quiet.

See the [design](../../docs/design/plugins/Migration.md),
[implementation plan](../../docs/plan/analysis/legacy-migration.md) and
[CLI contract](../../docs/design/cli-commands.md#migration-interaction).
Integration tests use disposable homes and the unchanged native consumers. The CLI
flow covers independent distribution routing, export, query and retry.
