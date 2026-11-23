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
let job = jobs
    .create(CreateJob {
        job: Job {
            name: name.into(),
            schedule,
            prompt,
            target: Target::Session { session_id, branch_id, input_mode },
            ..Job::new()
        },
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
        branch_id,
        prompt,
        every_seconds: None,
        first_delay_seconds: Some(delay_seconds),
    })
    .await?;
let armed = loops.next(NextLoop { session_id, loop_id: record.loop_id.clone(), delay_seconds }).await?;
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

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `list` | The jobs as stored now | `{}` | `{jobs}` | None | none |
| `get` | One job | `{jobId}` | `{job}` | None | `not_found` |
| `create` | Add one job | `{job}` | `{job}` | None; accepted work completes | `invalid`, `conflict`, `storage`, `unsynced`, `closed` |
| `update` | Replace one job's fields | `{jobId, job}` | `{job}` | None; accepted work completes | `invalid`, `not_found`, `conflict`, `storage`, `unsynced`, `closed` |
| `delete` | Remove one job | `{jobId}` | `{}` | None; accepted work completes | `not_found`, `conflict`, `storage`, `unsynced`, `closed` |
| `setEnabled` | Enable or disable one job | `{jobId, enabled}` | `{job}` | None | `not_found`, `conflict`, `storage`, `unsynced`, `closed` |
| `runNow` | Accept one window now | `{jobId}` | `{run}` | None; the window settles | `invalid`, `not_found`, `closed` |
| `history` | One job's delivery records | `{jobId, limit?}` | `{runs}` | None | `invalid`, `not_found` |
| `createWakeup` | Record one one-shot input | `{sessionId, branchId?, at, prompt}` | `{wakeupId}` | None; the record write completes | `invalid`, `storage`, `closed` |
| `listWakeups` | One Session branch's wakeups | `{sessionId, branchId?}` | `{wakeups}` | None | `invalid`, `storage`, `closed` |
| `cancelWakeup` | Settle a scheduled wakeup | `{sessionId, wakeupId}` | `{state?}` | None; the cancel settles the record | `wakeup_not_found`, `invalid`, `storage`, `closed` |
| `startLoop` | Record a loop and its first due time | `{sessionId, branchId?, prompt, everySeconds?, firstDelaySeconds?}` | `{loop}` | None | `invalid`, `loop_limit`, `loop_invalid`, `storage`, `closed` |
| `nextLoop` | Arm the following round | `{sessionId, loopId, delaySeconds}` | `{loop}` | None | `loop_not_found`, `loop_invalid`, `storage`, `closed` |
| `stopLoop` | End a loop | `{sessionId, loopId}` | `{loop, settled}` | None | `loop_not_found`, `storage`, `closed` |
| `listLoops` | One Session branch's loops | `{sessionId, branchId?}` | `{loops}` | None | `invalid`, `storage`, `closed` |

A job's target is data: `{type: "session", sessionId, branchId, inputMode?}` or `{type: "launch", cwd, agentId?, policy?, maxRunMinutes?}`. Only a management create or update may set a launch target; the tool's create and update set the calling Session and branch. A job stores its revision; an update reads the stored job under the Settings change and replaces the fields the caller sent. The job's submission ids are `scheduler:<runId>`, `wakeup:<wakeupId>` and `loop:<loopId>:<round>`.

The `scheduleJobs` face has these eight operations and no `call` action menu. The `schedule.*` namespace maps `list` → `list`, `save` → `create` or `update`, `delete` → `delete`, `set_enabled` → `setEnabled`, `run_now` → `runNow`, `history` → `history`; `watch` is removed. The `schedule` tool maps its `list`/`get`/`create`/`update`/`delete`/`set_enabled`/`run_now`/`history` actions to the same eight operations.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | Fix the job, the schedule, the prompt, the instant or the target |
| `not_found` | The job does not exist; list first |
| `conflict` | The document or job revision changed between the caller's read and write; read again and retry |
| `storage` / `unsynced` | The record was not written, or its durability is unconfirmed; a repeated read decides |
| `closed` | The Scheduler generation is unloading; the next generation serves the operation |
| `wakeup_not_found` | No such wakeup of the calling Session; list first |
| `loop_not_found` | No such loop of the calling Session; list first |
| `loop_limit` / `loop_invalid` | Remove a live loop, or fix the prompt, delay or cadence |

The delivery's target-gone errors are not job-operation errors: a delivery that answers `session_closed`, `branch_missing` or `branch_ended` settles `failed`, disables a session-target job, cancels a wakeup and stops a loop.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| Job write | The Settings change reads the document, applies the caller's fields, checks the target and the job revision, writes and publishes the fact | Before the write; the change is dropped | The call ends; an accepted write completes | In-flight changes settle; the next generation reads the document at its next tick | Settings holds the document; Scheduler owns the job semantics |
| Job window | The timer reads the document, selects due jobs, records `delivering`, writes one input, settles the record, disables a gone target | The timer stops on unload; the record settles at the next load | — | In-flight deliveries finish; `delivering` records settle by `lookup` at the next start | Scheduler for the record; the target branch for the input |
| `runNow` | Record a window and deliver it like a due window | The timer stops; the record settles later | — | As a window | Scheduler |
| Launch window | `sessions.launch` with the stored policy, record the Session and stop time, write the input, stop at the deadline, delete at retention | The launch stops at its own deadline; a cancel does not stop the Session | The record keeps the deadline; the next generation's recovery stops the Session | New launches stop while the row is disabled; the next load stops an overdue Session | Agents owns the Session; Scheduler owns the deadline and the record |
| Wakeup fire | The pass reads the file, takes `scheduled` records inside the grace, writes one input, settles `fired` | A cancel that settles first prevents the input; one after the input reports `fired` | The record stays; the next load re-evaluates | In-flight writes settle; the next pass reads the file | Scheduler |
| Wakeup cancel | Settle `cancelled` after the record write | — | — | — | Scheduler |
| Wakeup expiry | A record past the grace is `expired` unless its submission id is found; a gone target is `cancelled` | — | — | The next load settles it | Scheduler |
| Loop round | The pass reads the file, writes one queued input, advances the round or leaves the loop `waiting` | A stop settles the record; a pending round is not written | The record stays; the next load re-evaluates | In-flight writes settle; the next pass reads the file | Scheduler |
| Loop stop | Settle `stopped`; a terminal record is returned unchanged | — | — | — | Scheduler |
| Retention | Terminal records older than seven days are removed in the pass's write | — | — | The next load prunes | Scheduler |

A delivery or a job write takes a `processActivity` hold from before its external effect to its terminal record; a row unload does not release accepted work early. A disabled row starts no new windows, and its `delivering` records settle at the next load by `lookup` with the same submission id. A launch target started while the row was active is stopped at the next load if its deadline passed; the row does not start a new launch while disabled.

### 12. Loop

A loop is a Scheduler record whose rounds are ordinary queued inputs of one branch. Two cadences share one record: `self_paced` continues only when the model calls `next`, and `every_seconds` has a fixed window. A tool `start` requires `delay_seconds` from 60 to 3600 and delivers nothing at the call; the user's entries start with the first round due now. A record expires seven days after creation; a terminal record is removed seven days after it settled. The operations are `startLoop`, `nextLoop`, `stopLoop` and `listLoops`; the Session part maps the `loop` tool and `session.loop.*` to them.

## Exceptions with reasons

- A launch target's Session belongs to Agents; the Scheduler record owns the stop time and retention. The record is the settlement: `stop` at the deadline, `delete` at retention, and a disabled row's existing launch is stopped at the next load.
- The `schedule.*` namespace and the `wakeup`/`loop` tools are registration points, not facades: they map client or model arguments to the owner's operations, and the operations keep the business meaning.
- `session.wakeup.*` and `schedule.watch` are removed because no client ships; the `loops` and `wakeups` catalog names are removed because no cross-language caller exists; the `source: "Scheduler"` notices are removed because the Scheduler writes no notification. The `wakeup` tool gains `list` and `cancel`; `session.loop.*` stays because CLI, ACP and web call it.

## Verification

Flow tests on the owner:

- A due job writes one input with its submission id; a second window with the same id does not write twice.
- A job write under a competing Settings change answers `conflict`; a target-gone delivery disables a session job.
- A launch job starts, stops at its deadline and is deleted at retention; a disabled row stops an overdue launch at the next load.
- A wakeup fires once inside the grace, expires past it and is never replayed; a cancel before the write prevents the input.
- A loop round advances once, a self-paced loop waits without a fallback, a stop is idempotent, and an expired loop is not replayed.
- A restart settles `delivering` records by `lookup`; a terminal record is never changed.
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
use rutis::host::BoxFuture;

pub type JobId = String;
pub type WakeupId = String;
pub type LoopId = String;
pub type RunId = String;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: JobId,
    pub name: String,
    pub schedule: Schedule,
    pub prompt: String,
    pub target: Target,
    pub enabled: bool,
    pub revision: u64,
    pub created_by: Origin,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Target {
    Session { session_id: String, branch_id: String, input_mode: InputMode },
    Launch { cwd: String, agent_id: Option<String>, policy: Option<PermissionPreset>, max_run_minutes: Option<u32> },
}

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct SchedulerError {
    pub code: &'static str,
    pub message: String,
}

#[rutis::service(name = "scheduleJobs")]
impl Scheduler {
    pub async fn list(&self) -> Result<Jobs, SchedulerError>;
    pub async fn get(&self, request: GetJob) -> Result<Job, SchedulerError>;
    pub async fn create(&self, request: CreateJob) -> Result<Job, SchedulerError>;
    pub async fn update(&self, request: UpdateJob) -> Result<Job, SchedulerError>;
    pub async fn delete(&self, request: DeleteJob) -> Result<(), SchedulerError>;
    pub async fn set_enabled(&self, request: SetEnabled) -> Result<Job, SchedulerError>;
    pub async fn run_now(&self, request: RunNow) -> Result<Run, SchedulerError>;
    pub async fn history(&self, request: HistoryJobs) -> Result<Runs, SchedulerError>;
}

impl Scheduler {
    pub async fn create_wakeup(&self, request: CreateWakeup) -> Result<WakeupId, SchedulerError>;
    pub async fn list_wakeups(&self, request: ListWakeups) -> Result<Wakeups, SchedulerError>;
    pub async fn cancel_wakeup(&self, request: CancelWakeup) -> Result<WakeupState, SchedulerError>;
    pub async fn start_loop(&self, request: StartLoop) -> Result<Loop, SchedulerError>;
    pub async fn next_loop(&self, request: NextLoop) -> Result<Loop, SchedulerError>;
    pub async fn stop_loop(&self, request: StopLoop) -> Result<Loop, SchedulerError>;
    pub async fn list_loops(&self, request: ListLoops) -> Result<Loops, SchedulerError>;
}
```

The `schedule`, `wakeup` and `loop` tools register with `toolRegistry`; `session.loop.*` registers with `sessionMethods`; the guide entry registers with `guideEntries`.
