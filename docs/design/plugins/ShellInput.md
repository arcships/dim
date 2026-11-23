# ShellInput

Scope: session. Issue: #2026.

ShellInput owns the operator's shell in the Session's workspace: `shell.run`, `shell.stop` and `shell.list`, the run table, and the `<dim-shell-input>` entry that records a command's result. The current implementation is in `crates/dim-agent-capabilities/src/shell_input.rs`; the plugin page is [ShellInput](../../architecture/rust-agent/plugins/ShellInput.md).

## Use and scope: responsibility split

ShellInput owns:

- **Runs.** One run id per accepted command, its output capture, its progress and its terminal record; a run ends when its command settles.
- **The shell.** Where a command runs, how its output is captured, and when a command is stopped.
- **The input it writes.** A `record` run's result becomes an Append input on the line that asked, carrying the `<dim-shell-input>` entry; a `private` run's result goes only to the caller.
- **The Session's hold.** A running command holds the Session so an idle release does not end it.

Callers keep:

- When to run a command: the client casts `runId`, chooses `mode` and calls the method.
- What a command means: the operator writes it.
- The line's other input: the line's InputQueue decides order and idempotency.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A client runs a command in the Session's workspace | No: the run table, the capture, the hold and the result input are the owner's | `shell.run` |
| A client stops a running command | No: the run's process tree is the owner's | `shell.stop` |
| A client lists the running commands | No: the run table is the owner's | `shell.list` |
| A command runs when the Session is closing | No: the phase check and the hold are the owner's | `shell.run` answers `closed` |
| A second command runs beside the first | No: one run per Session is the owner's rule | `shell.run` answers `shell_busy` |

An operation or parameter exists only where the row says No.

## Caller example

A client runs a command, watches its progress and later stops it:

```rust
let RunReply { run_id, status, output, duration_ms, .. } = shell.run(RunParams {
    session_id: session.clone(),
    branch_id: Some(line.clone()),
    run_id: cast_run_id,
    command,
    mode: Mode::Record,
}, progress, cancelled.clone()).await?;
shell.stop(StopParams { session_id: session.clone(), run_id }, cancelled).await?;
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `P:HostPlatform` | Home, shell, environment and process supervision. |
| `S:sessionScope` | `config` for cwd, the collaboration role and the shell preference, `identity` for the Session id in the entry and the output path, and `phase`/`withHold` for the command's extent. |
| `S:sessionMethods` | Register the `shell` namespace until this part unloads. |
| `S:guideEntries` | The `<dim-shell-input>` entry, registered until this part unloads. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `shell.run` | Run one command and, in `record` mode, write its result as the line's input | `{sessionId, branchId?, runId, command, mode}` | `{runId, status, output, truncated, durationMs}`, with `progress` while it runs | The wait for the reply ends as `operation_interrupted`; the run stays with this generation and its result is still written | `invalid`, `shell_busy`, `closed` |
| `shell.stop` | Stop a running command | `{sessionId, runId}` | `{stopped}` | None; the stop settles | `invalid`, `closed` |
| `shell.list` | The commands running now | `{sessionId}` | `{runs: [{runId, branchId, mode, command, startedAt}]}` | None | `invalid`, `closed` |

`runId` is cast by the client and is the run's identity: a command never runs twice under one id, and a `shell.stop` that arrives before its run makes that run start nothing. `mode` is `record` (`!command`) or `private` (`!!command`). `branchId` is the line id; absent names the default line.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | The command is empty, the run id is reused with a different call, or the parameters are malformed |
| `shell_busy` | Another run of this Session is running; wait for it or stop it |
| `closed` | The Session or the line is closing; no command starts |
| `operation_interrupted` | The wait ended before the run did; the run continues and its result still arrives |

The wire codes stay the client's: `shell_busy`, `shell_unavailable`, `spawn_failed`, `operation_interrupted`, `service_reloading`, `invalid_params`.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `shell.run` | Check the phase, take the hold, admit the run id, spawn the process, stream progress, answer the run's result, write the `record` input, release the hold | The wait ends `operation_interrupted`; the run continues; a stop that arrived first leaves the run stopped | The run's process tree is released; the part's unload joins the runs | New runs are refused; running commands stop and settle | ShellInput for the run and the process tree; the line's queue for the input's order |
| `shell.stop` | Stop the run and settle it | Nothing | Nothing | Refused `closed` | ShellInput |
| `shell.list` | Read the run table | — | Nothing | Answers the state it observed | ShellInput |

## Exceptions with reasons

- The `record` input is written by this owner, not by the client: the command's output and the entry belong to the run, and the line's queue receives one input.
- Progress is a notification on the call, not a Timeline fact: a client that disconnects mid-run does not cancel the run; the result still lands.
- The hold is taken before the spawn and released after the input commits: an idle release must not end the Session between the command and its result.

## Verification

- `shell.run` answers the run's status, output, truncation and duration; a `record` run writes one input on the named line carrying `<dim-shell-input>`; a `private` run writes none.
- A cancelled wait ends `operation_interrupted` and the run continues; a `shell.stop` before the run starts makes it start nothing.
- `shell.stop` answers `{stopped}`; an unknown run id is `invalid`; `shell.list` answers the running set with the client fields.
- A Session closing before the spawn answers `closed`; a hold taken before the spawn keeps an idle release from ending the Session; a second run answers `shell_busy`.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part; it writes the line's input through the line's owner, not a Timeline event of its own. |
| §2 scopes | Session: the runs belong to the Session's workspace; the line is an id. |
| §3 orthogonality | No other owner's state is read; the line's queue is reached by id. |
| §4 rules 1–5, 9 | No command event, handoff, before callback, batch or Timeline fact of its own. |
| §4 rule 6 | The result reaches the model as the input this owner writes; a private run reaches only the caller. |
| §4 rule 7 | Callers use the three methods; no caller reads the run table. |
| §4 rule 8 | No infrastructure change. |
| §7 values, ids and callbacks | The run id, the line id and the output are data; progress is the call's notification; no process handle crosses. |
| §7 no proxy | The line's queue answers its input; this owner answers its runs. |
| §7 one interface | One name, `shell`, on the method table; no wrapper. |
| §7 cross-language | The methods are data; no method exists only for Rust. |
| §7 one change | The implementation task replaces the `TreeCommands` line lookup with the line access in one change. |
| ADR 002 D1 | Session scope for the runs; the line's input is the line's. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | The part depends on HostPlatform and the Session scope. |
| ADR 002 D4 | No setting selects the shell; the Session's preference is read at use. |
| ADR 002 D5 | The contract is data. |

## Requirements raised to mechanism owners

1. [#2024](https://github.com/arcships/dim/issues/2024)/[#2025](https://github.com/arcships/dim/issues/2025): `line_owner::<T>(line)` gives this Session part a branch owner's data face by id; registration points and lifecycle services are excluded.
2. [#2027](https://github.com/arcships/dim/issues/2027): the line's `inputQueue.submit` operation the `record` input is written through.

## Supersedes

- [session boundary A.1](../contracts/session-boundary.md): the `shell.*` method shapes stay; the part's access to a line changes from a `Branch` lookup to `line_owner::<T>(line)`.

## Binding appendix

The `shell` namespace is registered with `sessionMethods.register`; the handlers are callbacks. The run table is private. `CancelledFn` is the binding's cancellation argument and is not a request field.

```rust
use crate::host::CancelledFn;

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode { Record, Private }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunParams {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<String>,
    pub run_id: String,
    pub command: String,
    pub mode: Mode,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReply {
    pub run_id: String,
    pub status: String,
    pub output: String,
    pub truncated: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopParams { pub session_id: String, pub run_id: String }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopReply { pub stopped: bool }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListParams { pub session_id: String }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunInfo { pub run_id: String, pub branch_id: String, pub mode: Mode, pub command: String, pub started_at: u64 }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListReply { pub runs: Vec<RunInfo> }
```
