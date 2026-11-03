# Scheduler

Optional process and Session plugins implementing the accepted
[Scheduler contract](../../../docs/design/plugins/Scheduler.md). The process owns
jobs, windows, run tracking, reports and retention. The Session part offers
`schedule` to interactive leader Sessions. All execution uses the existing
`sessions`, `sessionPermissions`, `settings` and `pluginFiles` services.

## Install

The plugin directory carries [dim-plugin.json](dim-plugin.json) and is a default plugin
listed in [runtime/default-plugins.json](../../default-plugins.json), which PluginManager
loads (`--defaults`). An embedding host may still build a row from the manifest's `code`
fields for `SessionTypeScriptConfig.rows`. The plugin is that one row; from its
`apply` it registers its Session part (`session.ts`, as `SchedulerSession`) with
`partRegistry`, and each Session mounts a copy.
The structural configuration is `{}`. The manifest declares the same
`inject` dependencies as its module, matching the Workflow package. It
declares the `schedule` Settings source with user file
`schedule.json`, no project source and protected `/max_concurrent_runs`.

The package uses rutis 0.17.1, the host's pinned runtime; its TypeScript code
is unchanged from 0.16.0 ([upgrade contract](../../../docs/design/rutis-0.16-upgrade.md)). Row-level
dependencies stay declared. Invalid run data fails the owner and preserves the
original bytes; calls return `service_unavailable`. The native invalid-record
acceptance checks this behavior without a package-level loader workaround.

PluginManager creates `scheduler/` under the plugin storage root when it turns
the plugin on. A host that builds the row itself provisions that directory before
enabling the row: the native file transaction does not create parent directories.
Agents idle unloading
[#126](https://github.com/arcships/dim/issues/126) is delivered in main by
[#331](https://github.com/arcships/dim/pull/331); Scheduler uses its existing
Session holds and does not implement an unload loop.

## Public operations

The `schedule` tool supports `list`, `get`, `create`, `update`, `delete`,
`set_enabled`, `run_now` and `history`. Six protected local management methods
are exposed: `schedule.list/save/delete/set_enabled/run_now/history`. List and
history use existing report interruption; mutations drain admitted work.

Each authorizing create/update captures live authority and replaces the saved
origin and permission cap together, including rename-only updates. The result
shows the old and new authority and report destination. Management saves use
host authority and default to read-only execution. Tool saves use their saving
branch. Reuse checks the target's actual complete policy against both the saved
effective policy and the saving branch's current bound, without mutating it.

Jobs are global to this host, including across workspaces. Authorizing job edits use one
Settings document CAS; a conflict is returned without retry. Disabling or running
an existing job preserves its saved origin and cap. Accepted runs retain their
original authority, deadline and report destination through later edits.
Saving rejects a run budget whose deadline exceeds the inner-v1 four-digit UTC
timestamp range. Invalid raw jobs and per-job settlement failures are isolated
during window evaluation so other jobs can continue.

The timer derives due windows from the enabled anchor, current wall clock and
durable history. It supports numeric five-field cron with IANA zones, fixed
minute intervals and one-off instants. Missing DST wall times are skipped and
repeated wall times run once. After a gap, only the latest window can fire within its 60-second grace;
an older latest window becomes one missed record without catch-up. The current phase uses only the control node.
Window evaluation uses Settings changes, minute changes, the next in-minute
window deadline and accepted-run settlement. The one-second timer does not read
Settings or the run store on idle ticks. Active histories keep their one-second cadence. A raw invalid cron is exposed
as that job's `schedule_error` and does not stop other jobs.

Tool list results omit prompt bodies. Other job results include a UTF-8 prompt
preview of at most 4 KiB and `prompt_truncated`; names use at most 256 bytes and
run reasons use at most 512 bytes. Job/run identities and authorizing-save
transitions remain model-visible. Management and structured records retain the
full data. These are per-field limits; list size still scales with job count.

## Persistence and lifetime

`scheduler/runs.json` contains inner `{version:1,runs:[...]}` JSON in the native
S10 outer-v1 envelope. Missing files start empty. Invalid versions, malformed
records and incomplete receipt/ownership combinations fail activation with
`invalid_data`, preserving the original bytes. There is no migration.

Run admission writes `launching` before effects. Actual launch/reuse policy and
identity are recorded before input. The worker checks the same saved job revision
and fixed deadline before both admission and input, then tracks only its own
input through published history at one-second intervals. Recovery uses lookup
and history; it never relaunches or resubmits accepted work.
A failed Settings pre-effect recheck skips that run with `settings_read_failed`
without faulting unrelated trackers. A thrown history read retries the same
published prefix. Already-applied unconsumed input remains pending without
repeated withdrawal; later consumption or interruption decides its outcome.

Tool-saved completed, failed, cancelled, timed-out and lost runs attempt one
Steer report to their recorded saving branch. Skipped/missed runs and every
management-saved run retain history only. Delivery failure records a diagnostic;
terminal records are never replayed. The run retains its scope through admitted
report completion and diagnostic writes before cleanup can finish.

Retention keeps active/unsettled runs and the newest 100 terminal runs per
existing job. Only successful launch receipts confer cleanup ownership. A
referenced reuse Session keeps its last launch record. Native deletion precedes
forgetting its final ownership record; failure keeps that record for retry,
including when no jobs remain. An unreadable Settings document does not remove
references. Native file transactions serialize the store; no JavaScript lock or
separate cleanup queue is added.
Retention reads the run snapshot before current job references: a run's job is
committed first, so a newer run cannot be mistaken for an orphan against older
configuration. Cleanup removes only its selected expired IDs.
Successful retention stops its retry checks. A failed read, deletion or record
removal schedules another pass after a minute through the existing timer.

## Verification

Run from the repository root, using Node 24:

```sh
npm ci --prefix runtime/plugins/scheduler --ignore-scripts
node runtime/node_modules/typescript/bin/tsc -p runtime/plugins/scheduler/tsconfig.json
cargo test --locked -p dim-app-server --test transport_flow node_scheduler -- --ignored
```

Before native flows, reinstall the local packages using the CI
`npm install --no-save --ignore-scripts --install-links` step in
[ci.yml](../../../.github/workflows/ci.yml). Native rows resolve the installed
package copy; source edits alone do not refresh it.

| Public flow | Evidence |
| --- | --- |
| Actual source defaults/protected fields, tool save, later live widening bounded by the saved cap, non-interactive execution, takeover, reporting and inner/outer-v1 records | `node_scheduler_saves_live_authority_runs_noninteractive_and_reports_to_the_latest_saver` |
| An untitled interactive leader adopts its saving operator input before the raw Steer report; subsequent automatic title generation sends the original operator text to the real auxiliary HTTP binding and publishes its title | [`scheduler_reports_do_not_replace_the_first_operator_input_for_titles`](../../../crates/dim-session-title/tests/scheduler_flow.rs) |
| Required invalid-record startup, unchanged bytes, failed optional owner and usable native Sessions | `node_scheduler_invalid_inner_records_preserve_bytes_and_do_not_block_native_sessions` |
| Agents index replacement keeps the owner and run loaded; owner reload preserves pending approval, input, deadline and anchor; completion/report occur once | `node_scheduler_owner_reload_tracks_the_existing_native_run_without_replaying_input_or_reports` |
| Native timer windows, DST gap/fold, enabled-anchor changes, one-off disable and missed-window collapse | `node_scheduler_native_clock_preserves_anchors_collapses_gaps_and_handles_dst_once` |
| Overlap/capacity skip and slot release; busy reuse waits for its own consumption and turn end | `node_scheduler_overlap_capacity_and_busy_reuse_follow_native_input_boundaries` |
| Held-input timeout withdraws only its input; consumed-input timeout stops its turn; user stop remains cancelled | `node_scheduler_native_deadlines_withdraw_held_input_and_stop_only_consumed_work` |
| Late actual launch/reuse/live-policy replies retain identity and policy, without input, withdrawal or stop after the deadline | `node_scheduler_late_native_admission_and_policy_replies_preserve_identity_without_input` |
| Complete host restart before/after first-reuse binding uses only the actual Settings binding; unknown input/launch identity settles lost, committed input recovers, terminal reports never replay | `node_scheduler_complete_host_restart_reconciles_input_and_never_replays_terminal_reports` |
| An admitted native report held across owner or Node replacement lands once before deleted-job retention forgets its records | `node_scheduler_report_lands_once_across_row_or_runtime_cleanup` |
| Real filesystem deletion/replace failures preserve the last ownership record and retry with no jobs; external Sessions survive | `node_scheduler_retention_retries_real_delete_and_record_failures_with_no_jobs` |
| Fresh and pre-interrupt history prefixes preserve applied inputs without model consumption, deletion or another stop; later reuse consumption cannot change the terminal record | `node_scheduler_applied_inputs_interrupted_before_consumption_expire_without_another_stop` |
| A stale history prefix followed by genuine notHeld re-reads native deletion, completion or consumption before deciding | `node_scheduler_not_held_rereads_native_deletion_completion_and_consumption` |
| Shared cap/reuse vectors and unrepresentable saves use real native policies; default-policy groups share one host while job identities stay separate | `node_scheduler_shared_policy_vectors_reach_native_save_launch_and_reuse` |
| Selected/other job edits, one-CAS conflict, frozen accepted prompt/authority/report target and accepted same-revision local writes | `node_scheduler_job_edits_cas_and_local_source_writes_preserve_accepted_snapshots` |
| Both job origins preserve completed/failed/cancelled/timed-out/skipped/missed history; only selected tool outcomes send one Steer report with model-visible identities, and reload sends none | `node_scheduler_reports_only_selected_tool_outcomes_with_model_visible_identity` |
| Closed/deleted report targets and a discarded reply after actual native publication preserve the outcome, record failure once and never redirect or resend after reload | `node_scheduler_failed_reports_keep_the_outcome_and_never_retry_or_redirect` |
| Missing reuse target fails and reports; unreadable starter authority skips. Both preserve binding/cap without input, relaunch or policy mutation | `node_scheduler_missing_reuse_target_fails_but_unreadable_authority_skips` |
| The newest 100 terminal records, shared/reuse references and final launch receipt survive native cleanup; forks/external Sessions and unreadable sources remain intact; restart reconciles deletion before record removal | `node_scheduler_retention_preserves_shared_receipts_forks_and_unreadable_sources` |
| Active runs and unsettled reports exceed the terminal limit; job deletion waits for their real completion/report settlement before deleting their Sessions | `node_scheduler_retention_keeps_active_runs_and_deleted_jobs_unsettled_reports` |

A prefix captured before a user interrupt can permit a withdrawal command at the deadline.
InputQueue returns `notHeld` for the applied input; the command does not imply an `input/deleted`
fact. The native check preserves this distinction and still requires no model call or extra stop.

The native matrix exercises the public process/Session services and runs with the repository's public Node binding setup in CI, including the required
invalid-record case. The test fixture wraps the real package's public ports
and uses Node's built-in test clock. Native SQLite publication and filesystem
permissions supply the report and retention faults; production interfaces are
unchanged. Strict typechecking includes this fixture.

Agents installs stored owners before publishing its index, as implemented in
[#327](https://github.com/arcships/dim/pull/327). The strict full-host flow verifies
five crash points: a launch reply before its identity is recorded, input before
admission, its committed reply, report input and report reply. Unknown launch
identity or input settles as `lost` without another Session or run input. Seven
scenarios cover both origins for those lost runs: configured tool origins report
once, while management origins retain only history without a delivery diagnostic.
Native history verifies the actual effects.

Retention fixtures derive valid persisted records from actual launch/reuse
receipts while the original owner is disabled and its writers have joined.
The checks retain 100 terminal records plus the final referenced launch receipt,
then release that receipt only after its references disappear. Actual SQLite
source corruption and file permissions exercise read failures without resetting
bytes. Deleting the referencing job through `schedule.delete` triggers retention
while the run file is unreadable, so its existing minute retry owns recovery. A real Node process is killed after native deletion but before record
removal; startup reconciles the absent Session without launching a replacement.
Separate live runs and held report calls verify retention above the terminal
limit and cleanup after job deletion. These checks preserve the existing run
format, public services and owner timer; no production test hook is added.

A failed timer or Settings-wake evaluation is logged; a per-job error leaves
the next job eligible. Failed ticks do not trigger retention. Unreadable
Settings or run files preserve their bytes and references;
cleanup retries on its minute cadence after the read recovers. The timer does
not mark the entire owner failed for a pre-effect read error. Accepted run
operations keep their existing storage-failure handling.
If another run settles during a cleanup pass, the existing retry deadline keeps
one follow-up pass pending so its newly expired receipt is not left behind.

Known contract limits remain: branch-addressed stop can interrupt a later turn;
adopted input cannot be retracted; a crash between terminal persistence and report
may omit the report; a launch reply lost before its Session identity is persisted
cannot be tracked or deleted by this plugin. These do not add core requirements.

First reuse persists the actual launch receipt before binding through Settings
and sends no input until that write is confirmed. One definitive conflict
rereads and revalidates the same enabled, unbound reuse job before retrying once.
A second definitive conflict still lets that eligible job run once in its
recorded Session, with `binding_conflict` in detail and no binding. A possibly
committed reply skips input and is not retried. The confirmed reply supplies only
the comparison revision; accepted prompt, authority and report target stay fixed.
Recovery reconciles the existing receipt and never repairs binding or resubmits
input. Later runs read their binding from Settings. Ordinary recorded-window
settlement disables one-off jobs; binding does not add a post-input disable write.
See [A.5.1](../../../docs/design/plugins/Scheduler.md#a51-first-reuse-binding-before-input).
