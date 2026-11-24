# Wait

Scope: session. Issue: #2026.

Wait owns the `wait` tool: the tool's declaration and the text the model reads when a wait ends. The line's arrival table and the single-wait slot are WaitBranch's (#2027; its contract lands with that issue); this document states what the tool calls.

## Use and scope: responsibility split

Wait owns:

- **The tool declaration.** Its schema, its two optional inputs and the result kinds the model reads.
- **The result text.** `wait ended: notification (<task ids>)`, `wait ended: steer`, `wait ended: interrupted`, `wait ended: timeout`, and the `already delivered` answer for a notice the model already read.
- **The refusal texts.** A second wait's `wait is already running in this session.` and the reload settlement.

Callers keep:

- When to wait: the model decides.
- What a task id means: BackgroundExec and Collaboration own their tasks.
- The arrival table and the slot: WaitBranch owns them; the tool only reads the result.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| The model waits on its line for a matching arrival | No: the arrival table and the single slot are the line owner's | the `wait` tool calling WaitBranch's `wait` by line id |
| The model asks for one task or any arrival | Yes: the same call with an optional task id | Business |
| A second wait runs beside the first | No: the slot is the line owner's; the tool turns `already_waiting` into its refusal text | Business mapping |
| Read what already arrived | Yes: the line's `wait` answers `already_notified`, or the notice is already in the model's context | Business |

An operation or parameter exists only where the row says No.

## Caller example

The tool runs on the calling line and asks the line's owner to wait:

```rust
let line = execution.branch_id; // a value
let WaitReply { reason, task_ids, waited_ms } = line_owner::<WaitBranch>(line)?
    .wait(WaitRequest { task_id, timeout_ms: 300_000 }, cancelled.clone())
    .await
    .map_err(|error| match error {
        WaitError::AlreadyWaiting => ToolResult::error("wait is already running in this session."),
        WaitError::Closed => return Err(cancelled_call("capability reloading")),
    })?;
let text = match reason {
    Reason::Notification => format!("wait ended: notification ({})", task_ids.join(", ")),
    Reason::UserSteer => "wait ended: steer".into(),
    Reason::AlreadyNotified => format!("wait ended: notification ({}) was already delivered above.", task_ids.join(", ")),
    Reason::Timeout => "wait ended: timeout".into(),
    Reason::Interrupted => "wait ended: interrupted".into(),
};
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:toolRegistry` | Register `wait` until this part unloads. |
| The line access and WaitBranch's `wait` | The arrival table and the slot, reached by line id (#2027). |

### Contributions

No service of its own; the `wait` tool is the whole surface.

| Contribution | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- |
| `wait` tool | `{taskId?, timeoutMs?}` | one `ToolResult`: the text above, `isError` for a refusal or a cancelled wait | The call's token; a dropped call releases the slot | mapped from the line's `already_waiting`/`closed` |

The tool's own input rules are its schema's: `timeoutMs` is 1 through 300,000, default 300,000; a non-string task id or an out-of-range timeout is an invalid-input result.

### Flows

| Path | Normal | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| Tool call | Register the tool, call the line's `wait`, map the reason to text, return the result | The line's `wait` answers `interrupted`; the tool returns that text | The dropped call releases the line's slot | The line's owner answers `closed`; the tool returns the reload settlement | WaitBranch for the slot and arrivals; ToolExecutor for the tool result's fact |

## Exceptions with reasons

- The tool's result texts are protocol: the model reads the task ids and the end reason, and a change to either changes what it can do next. A reload ends the call as cancelled with the text `capability reloading`, not as a tool result; the turn fails.
- A notice the model already read does not end a later wait: the line's owner answers `already_notified`, and the tool tells the model it was delivered.

## Verification

- A wait ends on the matching completion, not on the input that started the turn; a steer or a stop ends it; a second wait answers the refusal text.
- A notice the model already read answers `already delivered above`; a timeout answers `wait ended: timeout`.
- Cancelling the tool call ends the wait as interrupted and releases the slot; dropping the future releases the slot.
- A reload answers the settlement text; the line's next generation starts with no old arrivals.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part; it writes no Timeline fact; the arrival table is WaitBranch's branch Capability. |
| §2 scopes | The tool is Session; the table is branch; the tool reaches the table by line id. |
| §3 orthogonality | No other owner's state is read; the line owner answers the arrival. |
| §4 rules 1–5, 9 | No command event, handoff, before callback, batch or Timeline fact from the tool. |
| §4 rule 6 | The result reaches the model as the tool result. |
| §4 rule 7 | The tool calls the line owner's operation; no caller reads the arrivals. |
| §4 rule 8 | No infrastructure change. |
| §7 values, ids and callbacks | The line id and the task ids are data; the cancellation is an argument; no object crosses. |
| §7 no proxy | The line owner answers the arrival; the tool answers its text. |
| §7 one interface | No service of its own; the tool is the surface. |
| §7 cross-language | The tool's execute is a callback; its input and result are data. |
| §7 one change | The implementation task migrates the tool off `TreeCommands` in one change. |
| ADR 002 D1 | Session for the tool; branch for the arrivals. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | The tool depends on the line owner's operation. |
| ADR 002 D4 | The timeout default is the tool's; no setting. |
| ADR 002 D5 | The contract is data. |

## Requirements raised to mechanism owners

1. [#2024](https://github.com/arcships/dim/issues/2024)/[#2025](https://github.com/arcships/dim/issues/2025): the line access that lets the Session tool reach WaitBranch's `wait` by `(session, line)` id.
2. [#2027](https://github.com/arcships/dim/issues/2027): WaitBranch's `wait {taskId?, timeoutMs?}` with `{reason, taskIds?, waitedMs}` and `already_waiting`/`closed`, as its contract states.

## Supersedes

- [session boundary A.1](../contracts/session-boundary.md): the `wait` tool's registration; the tool calls WaitBranch's `wait` by line id. The WaitBranch face is #2027's contract.
- [tool-contributions §2.2](../../architecture/rust-agent/tool-contributions.md): the `wait` tool's registration shape.

## Binding appendix

No service. The tool registers through the tool contract; its input is `{taskId?, timeoutMs?}` and its result is one `ToolResult` whose content is the text above.
