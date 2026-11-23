# ShellInput

Scope: session. Issue: #2026.

ShellInput owns the operator's shell in the Session's workspace: `shell.run`, `shell.stop` and `shell.list`, the run table, and the `<dim-shell-input>` entry that records a command's result. The current implementation is in `crates/dim-agent-capabilities/src/shell_input.rs`; the plugin page is [ShellInput](../../architecture/rust-agent/plugins/ShellInput.md).

## Use and scope: responsibility split

ShellInput owns:

- **Runs.** One run id per accepted command, its output capture and its terminal record; a run ends when its command settles.
- **The shell.** Where a command runs, how its output is captured, and when a command is stopped.
- **The input it writes.** A command's result becomes an input on the line that asked, carrying the `<dim-shell-input>` entry.
- **The Session's hold.** A running command holds the Session so an idle release does not end it.

Callers keep:

- When to run a command: the client calls the method.
- What a command means: the operator writes it.
- The line's other input: the line's InputQueue decides order and idempotency.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A client runs a command in the Session's workspace | No: the run table, the capture and the hold are the owner's | `shell.run` |
| A client stops a running command | No: the run's process tree is the owner's | `shell.stop` |
| A client lists the running commands | No: the run table is the owner's | `shell.list` |
| The command's result reaches the model | No: writing the input is the owner's; the line's queue decides the order | the same run operation |
| A command runs when the Session is closing | No: the phase check and the hold are the owner's | `shell.run` answers `closed` |

An operation or parameter exists only where the row says No.

## Caller example

A client runs a command and later stops it:

```rust
let RunReply { run_id, output_path } = shell.run(RunCommand {
    branch_id: line.clone(),
    command,
    run_in_background: false,
    timeout_ms: 30_000,
}, cancelled.clone()).await?;
shell.stop(StopRun { run_id }, cancelled).await?;
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
| `shell.run` | Run one command and write its result as the line's input | `{branchId, command, runInBackground?, timeoutMs?}` | `{runId, outputPath?}` | The caller's token stops the command; the line's input is not written | `invalid`, `closed`, `cancelled` |
| `shell.stop` | Stop a running command | `{runId}` | `{}` | None; the stop settles | `invalid`, `closed` |
| `shell.list` | The commands running now | `{}` | `{runs}` | None | None |

`branchId` is the line id. The result input is written to that line's input owner by id; the entry's text is registered with `guideEntries`.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | The command is empty, the run id is unknown, or the line id is malformed |
| `closed` | The Session or the line is closing; no command starts |
| `cancelled` | The command stopped before its result was written; the run's process tree is released |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `shell.run` | Check the phase, take the hold, spawn the command, capture, write the input, release the hold | The command stops; the input is not written; the hold is released | The command's process tree is released; the part's unload joins the runs | New runs are refused; running commands stop and settle | ShellInput for the run and the process tree; the line's queue for the input's order |
| `shell.stop` | Stop the run and settle it | Nothing | Nothing | Refused `closed` | ShellInput |
| `shell.list` | Read the run table | — | Nothing | Answers the state it observed | ShellInput |

## Exceptions with reasons

- The result input is written by this owner, not by the client: the command's output and the entry belong to the run, and the line's queue receives one input.
- A background command is not started here; the tool's background mode is `exec`'s, and ShellInput's runs are the operator's shell commands.
- The hold is taken before the spawn and released after the input commits: an idle release must not end the Session between the command and its result.

## Verification

- `shell.run` writes the result as one input on the named line; a cancelled run writes nothing and releases its process tree.
- `shell.stop` ends a running command; an unknown or ended run id is `invalid`; `shell.list` answers the running set.
- A Session closing before the spawn answers `closed`; a hold taken before the spawn keeps an idle release from ending the Session.
- The entry is registered at apply and its text reaches the model through the input.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part; it writes the line's input through the line's owner, not a Timeline event of its own. |
| §2 scopes | Session: the runs belong to the Session's workspace; the line is an id. |
| §3 orthogonality | No other owner's state is read; the line's queue is reached by id. |
| §4 rules 1–5, 9 | No command event, handoff, before callback, batch or Timeline fact of its own. |
| §4 rule 6 | The result reaches the model as the input this owner writes. |
| §4 rule 7 | Callers use the three methods; no caller reads the run table. |
| §4 rule 8 | No infrastructure change. |
| §7 values, ids and callbacks | The run id, the line id and the output path are data; no process handle crosses. |
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

1. [#2024](https://github.com/arcships/dim/issues/2024)/[#2025](https://github.com/arcships/dim/issues/2025): the line access that lets this Session part write an input to the named line by `(session, line)` id.
2. [#2027](https://github.com/arcships/dim/issues/2027): the line's `inputQueue.submit` operation the result input is written through.

## Binding appendix

The `shell` namespace is registered with the method table; the handler is a callback. The run table is private.

```rust
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCommand {
    pub branch_id: String,
    pub command: String,
    #[serde(default)]
    pub run_in_background: bool,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReply {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopRun { pub run_id: String }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunList { pub runs: Vec<RunInfo> }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellError {
    Invalid { message: String },
    Closed,
    Cancelled,
}
```
