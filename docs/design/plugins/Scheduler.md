# Scheduler

Scope: process and Session. Issue: [#2023](https://github.com/arcships/dim/issues/2023).

Scheduler owns the jobs, wakeups and loops that deliver saved prompts into Sessions, the one timer that serves them, the delivery records, and the Session-side tools and methods. This document is the owner's target contract; the current implementation is in `crates/dim-agent-scheduler`, and the scan report and delivery comment are on #2023.

## Use and scope: responsibility split

Scheduler owns:

- **Jobs.** The user's `schedule` Settings source, the job definitions, their windows, their targets and their enabled state.
- **Deliveries.** One record per window in `scheduler/runs.json`: `delivering`, `delivered`, `failed`, `lost` or `missed`, its submission id, and its lookup-based settlement after a restart.
- **Wakeups.** One-shot inputs in `scheduler/wakeups.json`, their fire/cancel/expiry, and their retention.
- **Loops.** Session-scoped rhythms in `scheduler/loops.json`: rounds, cadence, waiting, stop, expiry and retention.
- **The timer and retention.** One one-second pass serves job windows, wakeups, loops, recovery and retention.
- **Launched Sessions.** A launch-target job starts a Session, records its stop time, stops it and deletes it at retention.
- **The Session entries.** The `schedule`, `wakeup` and `loop` tools and the `session.loop.*` methods.

Callers keep:

- The job's meaning: its name, schedule, prompt, target and input mode; the wakeup's instant and prompt; the loop's prompt and cadence.
- The client entry's own policy: which job or loop a client shows, and how it presents the delivery history.
- Permission and approval of the tools' calls.
- The business meaning of a target Session or branch.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| The model manages jobs in its Session | No: the job document, the caller's target default, the revision and the delivery records are the owner's | the job operations through the `schedule` tool |
| An operator manages jobs and reads history | No: the same owner state, through the management namespace | the job operations through `schedule.*` |
| The model creates, lists or cancels a wakeup | No: the record, the timer and the fire/cancel boundary are the owner's | the wakeup operations through the `wakeup` tool |
| A client starts, lists or stops a loop of its Session | No: the record, the rounds and the timer are the owner's | the loop operations through `session.loop.*` |
| A client reads a wakeup's state | No client ships; the model's tool covers it | out of target |
| A client watches job or delivery changes | No client ships; `list` and `history` are the reads | out of target |
| A job reaches its target branch | No: the write's submission id, lookup, record and guide are the owner's | the delivery flow; `sessions.input` |
| A job launches a Session | No: the launch, stop time and retention are the owner's record | the launch flow; `sessions.launch`, `stop`, `delete` |
| The job document is read or changed | No: the Settings source and its revision are Settings' | `settings.register`, `read`, `change` |

An operation or parameter exists only where its row says No.

## Caller example

The `schedule` tool creates a job for its own Session and branch; the management namespace uses the same operations.

```rust
// The tool's create: the caller's Session and branch are the target.
let JobReply { job } = jobs
    .create(CreateJob {
        job: JobInput {
            name: Some(name.into()),
            schedule: Some(schedule),
            prompt: Some(prompt),
            target: Some(Target::Session { session_id: session_id.clone(), branch_id: branch_id.clone(), input_mode }),
            ..JobInput::default()
        },
        revision: None,
        caller: Some(Caller { session_id, branch_id }),
    })
    .await?;
// The tool's list and history; the management namespace has the same reads.
let own = jobs.list(ListJobs).await?;
let runs = jobs.history(HistoryJobs { job_id: job.id.clone(), limit: Some(20) }).await?;
```

The `wakeup` tool creates, lists and cancels one Session's wakeups. The branch defaults to the Session's current default branch.

```rust
let created = wakeups
    .create(CreateWakeup { session_id, branch_id: Some(branch), at: instant, prompt })
    .await?;
let own = wakeups.list(ListWakeups { session_id, branch_id: None }).await?;
let settled = wakeups.cancel(CancelWakeup { session_id, wakeup_id: created.wakeup_id }).await?;
```

The `loop` tool starts a self-paced loop; `next` arms the following round. The `session.loop.*` methods start, list and stop through the same operations.

```rust
let record = loops
    .start(StartLoop {
        session_id,
        branch_id: Some(branch_id),
        prompt,
        every_seconds: None,
        first_delay_seconds: Some(delay_seconds),
    })
    .await?
    .loop;
let armed = loops.next(NextLoop { session_id, loop_id: record.loop_id.clone(), delay_seconds }).await?.loop;
let own = loops.list(ListLoops { session_id, branch_id: None }).await?;
```

A delivery writes one ordinary input. The owner keeps the record and the receipt; the branch owns the input.

```rust
let receipt = sessions
    .input(InputWrite {
        session_id,
        branch_id,
        mode,
        submission_id: format!("scheduler:{run_id}"),
        raw_blocks: vec![marker(job_name, run_id)],
        content: prompt,
    })
    .await?;
```

## Own contract

### Dependencies

- `sessions` ([#2020](https://github.com/arcships/dim/issues/2020)): `input` for a delivery, `lookup` for the submission id, `pending` for a busy branch, `launch`, `stop` and `delete` for a launch target, and `defaultBranchId` for an omitted branch.
- `settings` ([#2017](https://github.com/arcships/dim/issues/2017)): `register` for the `schedule` source, `read` for the document, and `change` for every job write.
- `pluginFiles` ([#2016](https://github.com/arcships/dim/issues/2016)): the by-name text update and read for `scheduler/runs.json`, `scheduler/wakeups.json` and `scheduler/loops.json`, under each file's lock.
- `processActivity` ([#2021](https://github.com/arcships/dim/issues/2021)): `activated` before background work, and `hold`/`release` around accepted deliveries.
- `managementMethods` ([#2021](https://github.com/arcships/dim/issues/2021)): `register` for the `schedule` namespace.
- `SessionScope` ([#2025](https://github.com/arcships/dim/issues/2025)): `config` and `identity` for the Session part. The deleted `SessionConfig` and `SessionIdentity` names are not used.
- `sessionMethods`, `toolRegistry` and `guideEntries` ([#2026](https://github.com/arcships/dim/issues/2026)): register `session.loop.*`, the three tools and the guide entry.

### Operations

The job operations are the `scheduleJobs` data face; the `schedule` tool and the `schedule.*` namespace both map to them. The wakeup and loop operations are the owner's operations used by its Session part; no catalog name exists because no cross-language caller exists.

The job, history and loop request and reply shapes are the shipped client shapes: `schedule.*`, `session.loop.*`, the `schedule` tool and the `loop` tool keep the same field names, values and error codes. A change to one is a client-visible change and needs the maintainer's confirmation.

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `list` | The jobs with their next window and last delivery | `{}` | `{jobs: [ScheduledJob]}` | None | `invalid_data`, `io`, `service_reloading`, `service_unavailable` |
| `get` | One job | `{job_id}` | `{job}` | None | `schedule_not_found`, `invalid_data`, `io` |
| `create` | Add one job from a patch | `{job: JobInput, revision?, caller?}` | `{job}` | None; the write completes before the reply | `schedule_invalid`, `schedule_conflict`, `session_closed`, `service_reloading`, `service_unavailable`, `rejected`, `invalid_data`, `io`, `locked` |
| `update` | Patch one job; absent fields keep the stored value | `{job_id, job: JobInput, revision?, caller?}` | `{job}` | None; the write completes before the reply | `schedule_invalid`, `schedule_not_found`, `schedule_conflict`, `session_closed`, `service_reloading`, `service_unavailable`, `rejected`, `invalid_data`, `io`, `locked` |
| `delete` | Remove one job and retire its terminal deliveries | `{job_id}` | `{job_id}` | None; the write completes before the reply | `schedule_not_found`, `schedule_conflict`, `invalid_data`, `io`, `locked` |
| `setEnabled` | Enable or disable one job | `{job_id, enabled}` | `{job}` | None; the write completes before the reply | `schedule_not_found`, `schedule_conflict`, `invalid_data`, `io`, `locked` |
| `runNow` | Record one manual window | `{job_id}` | `Run` | None; the window settles | `schedule_not_found`, `schedule_invalid`, `invalid_data`, `io` |
| `history` | One job's deliveries, newest first | `{job_id, limit?}` | `{runs: [Run]}` | None | `schedule_not_found`, `invalid_data`, `io` |
| `createWakeup` | Record one one-shot input | `{sessionId, branchId?, at, prompt}` | `{wakeupId}` | None; the record write completes | `invalid_params`, `session_closed`, `service_reloading`, `service_unavailable`, `invalid_data`, `io`, `locked` |
| `listWakeups` | One Session branch's wakeups | `{sessionId, branchId?}` | `{wakeups}` | None | `invalid_params`, `session_closed`, `service_reloading`, `service_unavailable`, `invalid_data`, `io` |
| `cancelWakeup` | Settle a scheduled wakeup | `{sessionId, wakeupId}` | `{state?}` | None; the cancel settles the record | `wakeup_not_found`, `invalid_params`, `invalid_data`, `io`, `locked` |
| `startLoop` | Record a loop and its first due time | `{sessionId, branchId?, prompt, everySeconds?, firstDelaySeconds?}` | `{loop}` | None | `invalid_params`, `session_closed`, `service_reloading`, `service_unavailable`, `loop_limit`, `loop_invalid`, `invalid_data`, `io`, `locked` |
| `nextLoop` | Arm the following round | `{sessionId, loopId, delaySeconds}` | `{loop}` | None | `loop_not_found`, `loop_invalid`, `invalid_data`, `io`, `locked` |
| `stopLoop` | End a loop | `{sessionId, loopId}` | `{loop, settled}` | None | `loop_not_found`, `invalid_data`, `io`, `locked` |
| `listLoops` | One Session branch's loops | `{sessionId, branchId?}` | `{loops}` | None | `invalid_params`, `session_closed`, `service_reloading`, `service_unavailable`, `invalid_data`, `io` |

A job's target is the shipped shape: `{kind: "session", session_id, branch_id?, input_mode?}` or `{kind: "launch", cwd, agent_id?, policy?, max_run_minutes?}`. `branch_id` defaults to `main`; `input_mode` defaults to `queue`; `max_run_minutes` defaults to 60. Only a management create or update may set a launch target; the tool's create and update pass `caller: Caller` and the owner fills a session target from it, refusing a launch target with `schedule_invalid`. The management save omits `caller` and carries the target explicitly. A job stores its revision; an update reads the stored job under the Settings change and patches the fields the caller sent. The job's submission ids are `scheduler:<runId>`, `wakeup:<wakeupId>` and `loop:<loopId>:<round>`.

The `scheduleJobs` face has these eight operations and no `call` action menu. The `schedule.*` namespace maps `list` → `list`, `save` → `create` or `update`, `delete` → `delete`, `set_enabled` → `setEnabled`, `run_now` → `runNow`, `history` → `history`; `watch` is removed. The `schedule` tool maps its `list`/`get`/`create`/`update`/`delete`/`set_enabled`/`run_now`/`history` actions to the same eight operations.

The job-view fields are `next_window?`, `schedule_error?` and `last_run?`; the create and update input is a patch (`name?`, `schedule?`, `prompt?`, `target?`, `input_mode?`, `enabled?`), never a stored `Job` with owner-derived `id`, `revision` or `created_by`. A create's `enabled` defaults to true. A save clears `disabled_reason`.

The schedule forms are the shipped untagged objects: `{cron, tz?}`, `{every_minutes}` and `{at}`. `tz` defaults to the host's local zone; a DST-gap time is skipped and a repeated time fires once. An `every` job's windows are `enabled_at + k * every_minutes` for k ≥ 1; a new or re-enabled job's first window is one interval later, and an enabled save preserves `enabled_at`. `at` is one UTC instant with an offset; after it fires or is missed the job is disabled. Missed windows collapse to one `missed` record; older windows are never replayed; granularity is one minute. `input_mode` defaults to `queue`; `max_run_minutes` defaults to 60; the tool can create at most 16 jobs per Session; a Session has at most 8 live loops; a job prompt is at most 32 KiB; a prompt preview is at most 4 KiB.

The `Job` document keeps its legacy fields (`mode`, `session_id`, `cwd`, `model`, `agent_id`, `policy`, `policy_cap`, `max_run_minutes`): they are decoded, preserved and never reported; a new save writes no legacy field. The delivery file keeps its old state names and optional `attempts`, `released` and `job_revision`.

### Errors

The wire code is the enum variant name after `rename_all = "snake_case"`; the `Rejected` variant carries the Settings change's own code in `reason`.

| Kind | Retry? | What the caller does next |
| --- | --- | --- |
| `schedule_invalid` | No | Fix the job, the schedule, the prompt, the instant or the target; nothing is written |
| `schedule_not_found` | No | The job does not exist; list first |
| `schedule_conflict` | Yes, after reading | The document or job revision changed; read the job and retry once with the new revision |
| `session_closed` | No | The Session or its default branch cannot be read; fix the target or stop |
| `service_reloading` / `service_unavailable` | Yes | The Sessions or Scheduler row is reloading/unavailable; retry when Active |
| `rejected` | No | The Settings change refused; act on `reason` and `message` |
| `invalid_params` | No | Fix a wakeup or loop request's shape or field |
| `invalid_data` / `io` / `locked` | Yes, after reading | The record file is unreadable, the I/O failed, or the file lock is held; retry after the file settles |
| `cancelled` | No | The call was cancelled before handover; nothing was written |
| `wakeup_not_found` | No | No such wakeup of the calling Session; list first |
| `loop_not_found` | No | No such loop of the calling Session; list first |
| `loop_limit` | No | Remove a live loop first; at most 8 loops per Session |
| `loop_invalid` | No | Fix the prompt, delay or cadence |

A delivery's target-gone answers are record outcomes, not job-operation errors: `session_closed`, `branch_missing` or `branch_ended` settles the record `failed`, disables a session-target job, cancels a wakeup and stops a loop.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| Job write | The Settings change reads the document, applies the caller's fields, checks the target and the job revision, writes and publishes the fact | Before the write; the change is dropped | The call ends; an accepted write completes | In-flight changes settle; the next generation reads the document at its next tick | Settings holds the document; Scheduler owns the job semantics |
| Job window | The timer reads the document, selects due jobs, records `delivering`, writes one input, settles the record, disables a gone target | The timer stops on unload; the record settles at the next load | — | In-flight deliveries finish; `delivering` records settle by the read-only `lookup` at the next start | Scheduler for the record; the target branch for the input |
| `runNow` | Record a window and deliver it like a due window | The timer stops; the record settles later | — | As a window | Scheduler |
| Launch window | `sessions.launch` with the stored policy, record the Session and stop time, write the input, stop at the deadline, delete at retention | The launch stops at its own deadline; a cancel does not stop the Session | The record keeps the deadline; the next generation's recovery stops the Session | New launches stop while the row is disabled; the next load stops an overdue Session | Agents owns the Session; Scheduler owns the deadline and the record |
| Wakeup fire | The pass reads the file, takes `scheduled` records inside the grace, writes one input, settles `fired` | A cancel that settles first prevents the input; one after the input reports `fired` | The record stays; the next load re-evaluates | In-flight writes settle; an uncertain reply keeps the record `scheduled` and retries within the grace; a past-grace record settles `expired` | Scheduler |
| Wakeup cancel | Settle `cancelled` after the record write | — | — | — | Scheduler |
| Wakeup expiry | A record past the grace is `expired` unless its submission id is found; a gone target is `cancelled` | — | — | The next load settles it | Scheduler |
| Loop round | The pass reads the file, writes one queued input, advances the round or leaves the loop `waiting` | A stop settles the record; a pending round is not written | The record stays; the next load re-evaluates | The forward pass reads `sessions.pending` when it has `last_input_id`; the recovery pass settles a stored `delivering_round` by the read-only `lookup` with `loop:<loopId>:<round>` | Scheduler |
| Loop stop | Settle `stopped`; a terminal record is returned unchanged | — | — | — | Scheduler |
| Retention | Terminal records older than seven days are removed in the pass's write | — | — | The next load prunes | Scheduler |

Recovery is read-only and never repeats the input. A `delivering` record is settled by `sessions.lookup {sessionId, branchId, submissionId}`; the answer `accepted` settles `delivered` with the input's id and sequence. `unknown`, `reloading`, `ambiguous` and a transport failure carry no fact: the record stays `delivering`, and each answer counts toward a persisted five-attempt bound. `unknown` is final only for a write that returned before the lookup began; an admitted write whose reply was lost keeps running to its own outcome, so `unknown` cannot settle a record. Each pass persists the incremented `attempts` before it sends the lookup, and a crash between the persist and the lookup still counts that attempt. A record whose stored `attempts` is already five settles `lost` with `detail: lookup_budget_exhausted` before any lookup; the fifth answer without a fact settles `lost` with `detail` naming that answer. `session_closed`, `branch_missing` and `branch_ended` settle the record `failed` and disable the session-target job, cancel the wakeup or stop the loop. A launch record whose Session was never created settles `lost` with `detail: launch_reply_unknown`. The record's stored `job_revision` decides the target-gone disable; a record without it never disables its job. A row reload while an accepted input is in flight is the same case: the first lookup may answer `unknown`, the record stays `delivering`, and a later `accepted` settles it `delivered`; the verification covers it.

The lookup acquires the target Session: a cold Session is restored by Agents, and the delivery's admission covers the restore and the lookup. Admission: the hold is taken before the record's external effect and released after the terminal record. A row unload does not release the old hold; it stays until process shutdown, and the next generation takes a separate hold per unsettled record and releases that one after the terminal record. The next generation reconstructs the unsettled set from the `delivering` records before its first window; `activated` gates the first pass. Scheduler never holds a Session handle across the lookup or the delivery.

The forward loop pass reads `sessions.pending` for the loop's branch when it has `last_input_id`; a recovered `delivering_round` has no input id, so the recovery pass settles it by the read-only `lookup` with `loop:<loopId>:<round>`. An inconclusive forward round is left for the next window; a pending round is not written again. A wakeup fire that gets a definite refusal settles `cancelled`; an uncertain reply keeps the record `scheduled` and retries within the grace; a record past the grace settles `expired` and is never replayed. A job window whose write is uncertain follows the delivery rule above.

A delivery or a job write takes a `processActivity` hold from before its external effect to its terminal record; a row unload does not release accepted work early. A disabled row starts no new windows, and its `delivering` records settle at the next load by the read-only `lookup` with the same submission id. A launch target started while the row was active is stopped at the next load if its deadline passed; the row does not start a new launch while disabled.

### 12. Loop

A loop is a Scheduler record whose rounds are ordinary queued inputs of one branch. Two cadences share one record: `self_paced` continues only when the model calls `next`, and `every_seconds` has a fixed window. A tool `start` requires `delay_seconds` from 60 to 3600 and delivers nothing at the call; the user's entries start with the first round due now. A record expires seven days after creation; a terminal record is removed seven days after it settled. The operations are `startLoop`, `nextLoop`, `stopLoop` and `listLoops`; the Session part maps the `loop` tool and `session.loop.*` to them. `stop` returns `{loop, settled}`: `settled` is false while an accepted round's input may still be unsettled (the record carries `delivering_round`), and the recovery pass settles it by the read-only `lookup`; clients show "still settling" only for a non-terminal state with `settled: false`.

## Exceptions with reasons

- A launch target's Session belongs to Agents; the Scheduler record owns the stop time and retention. The record is the settlement: `stop` at the deadline, `delete` at retention, and a disabled row's existing launch is stopped at the next load.
- The `schedule.*` namespace and the `wakeup`/`loop` tools are registration points, not facades: they map client or model arguments to the owner's operations, and the operations keep the business meaning.
- `session.wakeup.*` and `schedule.watch` are removed because no client ships; the `loops` and `wakeups` catalog names are removed because no cross-language caller exists; the `source: "Scheduler"` notices are removed because the Scheduler writes no notification. The `wakeup` tool gains `list` and `cancel`; `session.loop.*` stays because CLI, ACP and web call it.

## Supersedes

- `docs/design/plugins/Scheduler.md` sections 1–12 and Appendices A–G (the old feature design) are replaced by this contract.
- `docs/design/service-faces.md` §3.2's `scheduleJobs`, `wakeups` and `loops` rows, and §4.3/§4.7/§4.7b: the eight job operations on `scheduleJobs`; the wakeup and loop operations are in-process and have no catalog name.
- `docs/design/contracts/session-boundary.md`'s Session namespace list: `session.wakeup` is removed; `session.loop` is the Scheduler entry; the `schedule.watch` progress sentence is removed.
- The Scheduler paragraphs in `docs/design/overview.md`, `docs/design/cli-commands.md`, `docs/design/appserver/sessions-service.md`, `docs/design/appserver/management-methods.md`, `docs/design/p6-p7-bindings.md`, `docs/design/web-workspace.md` and `docs/design/process-typescript.md`.
- The `scheduleJobs`, `wakeups` and `loops` catalog and README rows.

## Client-visible changes

- `session.wakeup.*` and `schedule.watch` are removed; no shipped client called them. The `schedule.*` methods except `watch`, `session.loop.*`, the `schedule` tool and the `loop` tool keep their wire names and shapes.
- The `wakeup` tool gains `list` and `cancel`; its model-visible description changes accordingly, and the flow test asserting the tool declaration is updated in the same change. The published `schedule`/`loop` descriptions and the EventHub A.4/A.6 texts are unchanged.
- The `source: "Scheduler"` Session notices are removed; no shipped client consumed them.

## Verification

Flow tests on the owner:

- A due job writes one input with its submission id; a second window with the same id does not write twice.
- A job write under a competing Settings change answers `conflict`; a target-gone delivery disables a session job.
- A launch job starts, stops at its deadline and is deleted at retention; a disabled row stops an overdue launch at the next load.
- A wakeup fires once inside the grace, expires past it and is never replayed; a cancel before the write prevents the input.
- A loop round advances once, a self-paced loop waits without a fallback, a stop is idempotent, and an expired loop is not replayed.
- A restart settles `delivering` records by the read-only `lookup`; a terminal record is never changed; `unknown`/`reloading`/`ambiguous`/transport keep it `delivering`, `attempts` persists, and the fifth answer settles `lost`.
- A row reload with an accepted input in flight: the first lookup may answer `unknown`, the record stays `delivering`, and a later `accepted` settles `delivered`; the old hold stays until process shutdown and the next generation takes its own hold for the record.
- A loop round crashes after its input commits: the record carries `delivering_round`, the recovery pass settles it by the read-only `lookup` with `loop:<loopId>:<round>`, and no second input is written.
- `every` first window is one interval after `enabled_at`; a re-enabled job keeps or resets it per the contract; a DST gap and a missed window collapse to one `missed`; `at` disables after firing.
- The 16-job, 8-loop, 32 KiB prompt and 4 KiB preview bounds; the 5-attempt lookup bound; the 100-record history; the CAS `schedule_conflict` retry; a legacy `schedule.json` and `runs.json` decode as written.
- The `wakeup` tool creates, lists and cancels; `session.loop.*` starts, lists and stops; the `schedule` tool maps to the job operations.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | Scheduler is one process part and one Session part. It writes no Timeline fact and folds no state, so it is not a Capability. |
| §2 scopes | Jobs, deliveries, wakeups, loops and the timer are process state; the tools, methods and guide entry are Session state. |
| §3 orthogonality | Scheduler reads no other plugin's state; it calls `sessions`, `settings` and `pluginFiles` operations and registers its own entries. |
| §4 rule 1 | A delivery writes an input through `sessions.input`; InputQueue and Loop decide what it becomes. |
| §4 rule 2 | A delivered input belongs to the target branch; Scheduler cannot withdraw it. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | The Timeline and the core carry no job, wakeup or loop meaning; the records are the owner's state. |
| §4 rule 5 | No Timeline batch. |
| §4 rule 6 | A delivery reaches the model only as a consumed input; a window that wrote nothing reaches no model. |
| §4 rule 7 | The dependencies are the operations above; no caller reads the owner's records. |
| §4 rule 8 | No mechanism requirement beyond the dependencies listed here. |
| §4 rule 9 | No Timeline event; the job document, delivery records, wakeups and loops are the owner's stores. |
| §7.2 ids, one face | The job operations are the `scheduleJobs` face; the wakeup and loop operations are used in-process by the Session part. A target is a Session and branch id; a delivery, wakeup or loop is an id. |
| §7.2 no proxy | `schedule.*`, `session.loop.*` and the tools are registration points, not facades; they map to the owner's operations. |
| ADR 002 D1 | Process for the jobs, records and timer; Session for the entries. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type; the Session part is the owner's own entry. |
| ADR 002 D3 | Scheduler depends on core process services; no core part depends on Scheduler. |
| ADR 002 D4 | The `schedule` source is business configuration in Settings; the rows are structural. |
| ADR 002 D5 | The contract is data and ids; the Rust binding is the appendix. |

§7.2 is the rule text of [#2029](https://github.com/arcships/dim/pull/2029) (open, head `a3c7c4d8a`); these contracts are reviewed against that head.

## Requirements raised to mechanism owners

- **Agents (#2020):** the `sessions` operations `input`, `lookup`, `pending`, `launch`, `stop`, `delete` and `defaultBranchId` as data operations, with `session_closed`, `branch_missing`, `branch_ended`, `reloading` and `transport` as the target-gone and retry answers. Scheduler does not take a `HostDispatch` object or decode the reply envelope.
- **Settings (#2017):** `register` for the `schedule` source, `read` and `change`; the change runs under the document's exclusion and carries no caller-held revision. Scheduler does not inject `settingsService`.
- **PluginFiles (#2016):** the by-name text update and read for the three files, with the lock held across the caller's change; no scope object.
- **ServerControl (#2021):** `activated`, `hold` and `release`; Scheduler does not hold a `ProcessHold` guard.
- **ManagementMethods (#2021):** `register` for the `schedule` namespace.
- **SessionScope (#2025):** `config` and `identity` for the Session part.
- **SessionMethods, ToolRegistry, GuideEntries (#2026):** the registration points for `session.loop.*`, the three tools and the guide entry.

## Binding appendix

The job operations are marked on the `Scheduler` owner type under the wire name `scheduleJobs`. `caller` is request data: the Session and branch a tool-created job targets. The wakeup and loop operations are Rust methods on the same owner type; the Session part calls them in process. `next` and the record's internal pass stay off the job face.

```rust
use dim_agent_runtime::PublicService;
use dim_agent_types::{InputMode, PermissionPolicy, PermissionPreset};

pub type JobId = String;
pub type WakeupId = String;
pub type LoopId = String;
pub type RunId = String;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobOrigin {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "tool")]
    Tool { session_id: String, branch_id: String },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Schedule {
    Cron(CronSchedule),
    Every(EverySchedule),
    At(AtSchedule),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CronSchedule {
    pub cron: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EverySchedule {
    pub every_minutes: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AtSchedule {
    pub at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    Session {
        session_id: String,
        #[serde(default = "default_branch_id")]
        branch_id: String,
        #[serde(default)]
        input_mode: InputMode,
    },
    Launch {
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        policy: Option<PermissionPreset>,
        #[serde(default = "default_max_run_minutes")]
        max_run_minutes: u32,
    },
}

fn default_branch_id() -> String { "main".to_owned() }
fn default_max_run_minutes() -> u32 { 60 }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisabledReason {
    pub code: String,
    pub at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Create,
    Reuse,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelSelection {
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub id: JobId,
    pub name: String,
    pub schedule: Schedule,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<DisabledReason>,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_at: Option<String>,
    pub revision: u64,
    pub created_by: JobOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PermissionPreset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_cap: Option<PermissionPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_run_minutes: Option<u32>,
}

/// The wire view the `schedule.*` and `schedule` tool reads: the stored job's
/// reported fields plus the computed view fields.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduledJob {
    pub id: JobId,
    pub name: String,
    pub prompt: String,
    pub schedule: Schedule,
    pub target: Target,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<DisabledReason>,
    pub revision: u64,
    pub created_by: JobOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_window: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<Run>,
}

/// The create/update patch; absent fields keep the stored value, and a create
/// never supplies `id`, `revision` or `created_by`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_mode: Option<InputMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Delivering,
    Delivered,
    Failed,
    Lost,
    Missed,
    #[serde(rename = "skipped")]
    Skipped,
    #[serde(rename = "launching")]
    Launching,
    #[serde(rename = "submitted")]
    Submitted,
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "cancelled")]
    Cancelled,
    #[serde(rename = "timed_out")]
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionOrigin {
    Launch,
    Reuse,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportTo {
    pub session_id: String,
    pub branch_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub run_id: RunId,
    pub job_id: JobId,
    pub window: String,
    pub state: DeliveryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_origin: Option<SessionOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PermissionPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_event_id: Option<String>,
    pub submission_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_to: Option<ReportTo>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u32>,
    #[serde(default, skip_serializing_if = "omitted_when_false")]
    pub released: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_revision: Option<u64>,
}

fn omitted_when_false(value: &bool) -> bool { !*value }

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeupState {
    Scheduled,
    Fired,
    Cancelled,
    Expired,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeupRecord {
    pub wakeup_id: WakeupId,
    pub session_id: String,
    pub branch_id: String,
    pub at: String,
    pub prompt: String,
    pub state: WakeupState,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<String>,
}

/// The wire view `session.wakeup.*` and the `wakeup` tool read.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Wakeup {
    pub wakeup_id: WakeupId,
    pub at: String,
    pub prompt: String,
    pub state: WakeupState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopState {
    Running,
    Waiting,
    Stopped,
    Expired,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Cadence {
    SelfPaced,
    Every { every_seconds: u64, anchor: String },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopRecord {
    pub loop_id: LoopId,
    pub session_id: String,
    pub branch_id: String,
    pub prompt: String,
    pub cadence: Cadence,
    pub state: LoopState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_at: Option<String>,
    pub round: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivering_round: Option<u64>,
    pub expires_at: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_input_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_round_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<String>,
}

/// The wire view `session.loop.*` and the `loop` tool read.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Loop {
    pub loop_id: LoopId,
    pub branch_id: String,
    pub state: LoopState,
    pub round: u64,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_round_at: Option<String>,
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListJobs {}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetJob { pub job_id: JobId }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Caller {
    pub session_id: String,
    pub branch_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateJob {
    pub job: JobInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<Caller>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateJob {
    pub job_id: JobId,
    pub job: JobInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<Caller>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteJob { pub job_id: JobId }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetEnabled { pub job_id: JobId, pub enabled: bool }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunNow { pub job_id: JobId }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryJobs {
    pub job_id: JobId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobsReply { pub jobs: Vec<ScheduledJob> }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobReply { pub job: ScheduledJob }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeleteJobReply { pub job_id: JobId }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunsReply { pub runs: Vec<Run> }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateWakeup {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<String>,
    pub at: String,
    pub prompt: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListWakeups {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelWakeup { pub session_id: String, pub wakeup_id: WakeupId }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartLoop {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<String>,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_delay_seconds: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NextLoop { pub session_id: String, pub loop_id: LoopId, pub delay_seconds: i64 }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StopLoop { pub session_id: String, pub loop_id: LoopId }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListLoops {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeupsReply { pub wakeups: Vec<Wakeup> }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWakeupReply { pub wakeup_id: WakeupId }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelWakeupReply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<WakeupState>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopReply { pub r#loop: Loop }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopLoopReply { pub r#loop: Loop, pub settled: bool }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopsReply { pub loops: Vec<Loop> }

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum SchedulerError {
    #[error("{message}")] InvalidParams { message: String },
    #[error("{message}")] ScheduleInvalid { message: String },
    #[error("{message}")] ScheduleConflict { message: String },
    #[error("{message}")] ScheduleNotFound { message: String },
    #[error("{message}")] WakeupNotFound { message: String },
    #[error("{message}")] LoopNotFound { message: String },
    #[error("{message}")] LoopLimit { message: String },
    #[error("{message}")] LoopInvalid { message: String },
    #[error("{message}")] SessionClosed { message: String },
    #[error("{message}")] ServiceReloading { message: String },
    #[error("{message}")] ServiceUnavailable { message: String },
    #[error("{message}")] InvalidData { message: String },
    #[error("{message}")] Io { message: String },
    #[error("{message}")] Locked { message: String },
    #[error("{message}")] Cancelled { message: String },
    #[error("{reason}: {message}")] Rejected { reason: String, message: String },
}

#[rutis::service(name = "scheduleJobs")]
impl Scheduler {
    pub async fn list(&self, request: ListJobs) -> Result<JobsReply, SchedulerError>;
    pub async fn get(&self, request: GetJob) -> Result<JobReply, SchedulerError>;
    pub async fn create(&self, request: CreateJob) -> Result<JobReply, SchedulerError>;
    pub async fn update(&self, request: UpdateJob) -> Result<JobReply, SchedulerError>;
    pub async fn delete(&self, request: DeleteJob) -> Result<DeleteJobReply, SchedulerError>;
    pub async fn set_enabled(&self, request: SetEnabled) -> Result<JobReply, SchedulerError>;
    // The request is received before the future is returned; a dropped reply does not stop the accepted window.
    pub fn run_now(&self, request: RunNow) -> impl std::future::Future<Output = Result<Run, SchedulerError>> + Send + '_;
    pub async fn history(&self, request: HistoryJobs) -> Result<RunsReply, SchedulerError>;
}

impl Scheduler {
    pub async fn create_wakeup(&self, request: CreateWakeup) -> Result<CreateWakeupReply, SchedulerError>;
    pub async fn list_wakeups(&self, request: ListWakeups) -> Result<WakeupsReply, SchedulerError>;
    pub async fn cancel_wakeup(&self, request: CancelWakeup) -> Result<CancelWakeupReply, SchedulerError>;
    pub async fn start_loop(&self, request: StartLoop) -> Result<LoopReply, SchedulerError>;
    pub async fn next_loop(&self, request: NextLoop) -> Result<LoopReply, SchedulerError>;
    pub async fn stop_loop(&self, request: StopLoop) -> Result<StopLoopReply, SchedulerError>;
    pub async fn list_loops(&self, request: ListLoops) -> Result<LoopsReply, SchedulerError>;
}
```

The `schedule`, `wakeup` and `loop` tools register with `toolRegistry`; `session.loop.*` registers with `sessionMethods`; the guide entry registers with `guideEntries`.
