# Workflow

Optional TypeScript package implementing the [Workflow contract](../../../docs/design/plugins/Workflow.md).
The plugin is one process row. It owns definitions, runs and report settlement, and registers its
Session part (`session.ts`, as `WorkflowSession`) with `partRegistry`; each Session's copy provides
the `workflow` tool through the declared `workflowRuns` dependency. All native I/O uses existing `sessions`,
`settings`, `workflowFiles` and `pluginFiles` ports.

## Loading

```sh
npm ci --prefix runtime --ignore-scripts
npm ci --prefix runtime/plugins/workflow --ignore-scripts
(cd runtime && npm install --no-save --package-lock=false --ignore-scripts --install-links ./plugins/workflow)
```

These commands set up the shared runtime and this package in the checkout for `--session-rows` embedding and the package's tests. `./dev` needs none of them: it stages the default plugins, with their own dependencies, into `staging/dev-plugins`.

The plugin directory carries [dim-plugin.json](dim-plugin.json) and is a default plugin listed in
[runtime/default-plugins.json](../../default-plugins.json), which PluginManager loads (`--defaults`).
An embedding host may still build a row from the manifest's `code` fields for
`ServerConfig.session_typescript.rows`. A Session without a
mount list (`SessionConfig.tools`) declares `workflow` once its Session part registers it; a Session with one
declares it when the list names it. PluginManager creates the product-root `workflow/`
directory when it turns the plugin on; a host that builds the row itself provisions it before
enabling the process row. A stage Session takes the stage agent's or default
profile's mount list: a profile without one exposes `workflow` inside a stage, and one whose list
omits it has no Workflow tool; Workflow does not add it to `sessions.launch`. When it is declared,
the non-interactive handler refuses invocation as A.2 requires. Missing parent directories fail with `io`; the plugin
does not create them through a filesystem fallback.

The process row's trusted `config.packages` is a list of unique `{id, root}` declarations. Roots
must already be canonical and authorized by the host. The native binding exposes only package ids
to the plugin. Model arguments and workflow files cannot grant file authority or install packages.
An unavailable dependency leaves its optional row Pending under existing assembly rules.

## Definitions and execution

`workflow.save` takes one of two strict branches. `{source:"user", document, expectedRevision?}`
saves the definition into the `workflow` Settings document with owner CAS: at `expectedRevision`
when the caller gives one, and at the revision the owner reads otherwise; a user save that names
`cwd` is refused. `{source:"project", cwd, document}` writes
`.dimcode/workflows/<name>.yaml` through `workflowFiles` and refuses `expectedRevision`, since the
file belongs to the workspace `cwd` names. Project definitions start disabled.
Only the user's local `settings.update` can change the protected
`projectEnabled[canonicalCwd][name]` field. An enabled definition takes effect at the next start;
a running workflow keeps its accepted definition and parameter snapshot.

```json
{
  "schema": "dim.workflow/v1",
  "name": "review",
  "description": "Analyze and summarize a target",
  "parameters": [{"name":"target","required":true}],
  "steps": [
    {"stage":{"id":"analyze","prompt":"Analyze {{params.target}}"}},
    {"stage":{"id":"summary","prompt":"Summarize {{stages.analyze.result}}"}}
  ]
}
```

Tool actions are `list`, `start`, `status` and `stop`. Management exposes the same operations
plus `save` as `workflow.*` methods. Start returns the run id; status returns stored stage results.
Tool `list` returns one `{definitions, truncated}` JSON object in `content`; description,
parameter and issue fields are bounded previews, entries stop at the 8 MiB payload and `truncated`
marks omitted definitions. Management `workflow.list` returns the same object.
Tool status/stop text includes stage identities/states and Session ids, one 4 KiB result preview
and the last 10 log entries; `structuredContent` retains the full stored run.
Tool status/stop require the caller Session to match the run's tool starter or report destination; unrelated Sessions receive `run_not_found` without record access or mutation. Management methods remain process-global.
The tool is refused in non-interactive Sessions. Existing Permission handles its unclassified risk:
ReadOnly denies, WorkspaceWrite asks and FullAccess allows.

Source precedence is enabled project, user, then plugin; invalid enabled definitions are reported
instead of silently falling through. Duplicate names across installed packages are invalid.
Templates accept only declared parameters and results of earlier steps, never sibling stages.
Supplied/default values are inserted literally; an omitted optional parameter inserts empty text.
Definition files have an 8 MiB read/replace limit so their base64 payload fits the existing 16 MiB bridge frame. Project saves check serialized YAML before a binary call and leave an existing file unchanged on rejection. Larger files are reported as invalid entries. One Workflow payload crossing the bridge is capped at 8 MiB of its serialized bytes: a new definition's compact JSON is capped at 1 MiB, and a user save that would make the `workflow` document exceed 8 MiB is rejected without writing. A user document written before these bounds is still read while it fits one frame; entries over 1 MiB show as invalid, and shrinking a document that stays over the payload is the protected `settings.update` client's operation. Parameters, including unused values and defaults, remain capped at 32 KiB each and 256 KiB total in UTF-8. Rejection precedes run persistence; existing inner-v2 snapshots remain decodable without these admission bounds.

Each stage runs in its own non-interactive Session. The actual first launch reply fixes the run's
permission ceiling before any sibling receives input. Management starts default to ReadOnly.
The Settings document contains `max_concurrent_runs` and per-run `max_concurrent_stages`, both
defaulting to 4; each new run reads their current values. A parallel step is rendered completely
before admission. A rendered prompt above 32 KiB fails that step without launching any sibling;
stored stage text is bounded to 64 KiB per stage and 512 KiB of committed JSON per run, each stored
log message and stage reason to 512 bytes of committed JSON, and report previews to 4 KiB.
Quote-heavy, backslash-heavy and control-character text is measured in that escaped form. Later
result text is truncated at a UTF-8 boundary to the run's remaining payload.

## Ownership and recovery

The process row tracks admitted calls, run tasks, native operations and file writes. Cleanup stops
new work, requests stage shutdown, joins its work and then closes the original file scope. History
tracking uses a fixed published prefix once per second and follows only each stage's own input.
Held inputs can be withdrawn. Stopping consumed work addresses the branch's current turn; the
existing later-turn race and retained applied input limits remain in force.

Completed, failed and interrupted runs report to their recorded destination as one raw Steer block;
explicit stops and management runs without a destination do not report. Before submission, the
inner run record persists `pending` with `attempt_open:true` (kept in the format; it decides
nothing). Every retry, including a replacement generation's recovery, resubmits the same
`submissionId`; the destination branch accepts it once and replies the first input's receipt.
An `ambiguous` reply (duplicates written before branches accepted each id once) settles the report
delivered and records a violation. Missing destinations are undeliverable.

Run storage is `workflow/runs.json`, with inner `{version:2,runs:[...]}` inside the host's S10 v1
wrapper. `workflow/runs.version` holds the token of the last committed store write: a running stage's
one-second wait reads that token (36 bytes) and reuses the owner's snapshot, so its per-second cost
does not grow with the store; only a changed token, startup recovery and every write read the store
itself. Runs admitted under the bounds below keep the text within the 8 MiB payload as the file
service carries it (the JSON string, where escapes can double): a start projects the store with
every active run's record, the stage identities, `agent_id` copies, results and logs it can gain
later, plus the new record; if that exceeds the payload it first expires the oldest settled
terminal runs and otherwise returns `limit_exceeded` without persisting. The retained store,
therefore, keeps each read, write and `workflow.status` reply within one bridge frame; a record
committed before these bounds keeps its original size. The token is written in its own transaction
after the store replace, so a crash between them leaves it naming older text; recovery reads the
store and does not depend on it. A failed token read or write is a storage failure, a failed store
access drops the snapshot so committed bytes stay visible, and a poll reaches the frame with the
token, never the store. Invalid, v1,
unknown-version or missing-marker records return `invalid_data` and preserve their bytes. There is
no migration. After a crash, unfinished runs become interrupted/lost without relaunch; a lost
stage's running turn or pending approval is left untouched. Only orderly unload stops stages under
A13. Active runs, pending reports and uncertain deletion records are retained; other terminal
history is limited to 100, and a start may expire older settled records for frame space. Owned
Sessions are deleted before removing their last ownership record.
A successful launch that stop overtakes before input remains named in its retained run; no turn
stop is sent without observed consumption. A.4's retained-reference calculation and 100-run policy
still apply, and #126 remains the loaded-idle-Session shipping prerequisite. An applied input that
was not withdrawn or interrupted may keep an explicit stop pending under §3.2; no new deadline
changes that accepted input/terminal-fact contract.

## Verification and delivery limits

```sh
node runtime/node_modules/typescript/bin/tsc -p runtime/plugins/workflow/tsconfig.json
cargo test --locked -p dim-app-server --test transport_flow node_workflow -- --ignored
```

Actual Node/App Server flows are in
[workflow.rs](../../../crates/dim-app-server/tests/transport_flow/workflow.rs), using public Node CI's
`DIM_TEST_NODE`, `DIM_TEST_JS_RUNTIME` and `DIM_TEST_RUTIS_SDK` settings and disposable homes.
They exercise stage Sessions, published reports, resubmission across row/runtime replacement, crashes and failed
record replacement. Native retention flows use actual Session deletion, file read/replace failures,
fixed-row replacement and the real maintenance callback. The fixture advances Node interval timers
without waiting one minute per pass; stage I/O, SQLite publication and filesystem faults remain real.
The read-failure retry case needs a valid startup restore before making the run
file unreadable. It advances both maintenance passes; corrupting that file before
row activation would exercise startup failure instead of retention recovery.
The existing [#126](https://github.com/arcships/dim/issues/126) idle-unload work must land before
shipping repeated stage creation. A launch whose reply is lost before its Session id is persisted
can leave an unidentified Session; it is not relaunched or recovered by a new registry.

## Acceptance coverage

The [native flows](../../../crates/dim-app-server/tests/transport_flow/workflow.rs) use real
Node rows, Settings, file authority, S2 calls, publication and Session history.

| Public behavior | Evidence |
| --- | --- |
| Definition and parameter admission, source identities, project enablement, source-file preservation and whole-parallel-step prompt rejection | `node_workflow_sources_stages_policy_and_report_use_the_real_package`, `node_workflow_project_enablement_invalid_sources_and_tool_policy` |
| Live first-admission policy, starter loss, frozen authority and native non-interactive stage Sessions | `node_workflow_captures_live_first_admission_and_survives_starter_and_index_changes` |
| Held, consumed and completed work settles on stop and reload | `node_workflow_stop_and_reload_settle_held_consumed_and_completed_work` |
| Reports wake wait and adopt after the start batch commits | `node_workflow_report_wakes_wait_and_is_adopted_after_its_start_batch` |
| Crash, row/runtime replacement, complete-host restart and uncertain report publication | `node_workflow_reconciles_crashes_failed_records_and_resubmitted_reports`, `node_workflow_reports_recover_after_complete_host_restart`, `node_workflow_background_report_resubmits_after_row_and_runtime_replacement` |
| Invalid inner records and failed change-token writes preserve committed runs | `node_workflow_rejects_invalid_inner_records_without_changing_bytes`, `node_workflow_failed_token_write_keeps_the_committed_run_readable` |
| Retention preserves live references, user forks and external Sessions, and retries real deletion/read/record faults | The three `node_workflow_retention_*` flows |

Scope cleanup also exercises a failing parallel reader while another native launch is admitted:
the original task joins its siblings before its file scope can close. No additional lock or owner
registry is used. The explicit installation and #126 shipping boundary above still apply.

Tool status and stop share a 4 KiB result preview and include only the last ten logs, each message bounded to 512 UTF-8 bytes. Full records remain in structured data and management status. Diagnostic messages are bounded to 4 KiB. A known owner reply, including its run id, survives tool retirement. A thrown issued call with an unknown outcome uses the existing uncertain-effects guidance; accepted work stays owned by the process row.
