# ServerControl

Scope: process. Issue: #2021.

ServerControl owns the process's publication gate, its shutdown intent and its admission of accepted work. `processActivity` is the one name this owner publishes; the current `serverControl` object and its second `processActivity` name are replaced. This document is the owner's target contract; the current implementation is in `crates/dim-app-server/src/control.rs`. The assembly that runs the shutdown sequence keeps its own sequence; Connection, Agents and Rpc are separate owners.

## Use and scope: responsibility split

ServerControl owns:

- **Publication.** Whether the host has opened: every required process row is Active and the runtimes released their rows. A row reload does not reset it.
- **Shutdown intent.** One request, its mode and its force flag. The first request wins; later requests are refused.
- **Accepted work.** Holds taken before work starts and released after the last owned effect settles. A hold keeps a shutdown graceful.
- **Listener faults.** The first listener exit becomes the process fault and begins the close.

Callers keep:

- The shutdown sequence: settling Agents, draining Rpc's commands, closing connections and answering the initiating client are the assembly's.
- What counts as accepted work: each holder takes a hold around its own effects.
- Session pauses: Agents owns them; ServerControl does not store another owner's guard.
- The client methods: `daemon.ping` and `daemon.shutdown` are Rpc's; this owner answers the intent they carry.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A process part must wait for the host to open before starting work | Yes: `activated` composes | `activated` |
| A part must stop when the daemon starts closing | Yes: `closing` composes | `closing` |
| A holder must stop a graceful close while its accepted work runs | No: only the owner sees every hold and can decide the intent; the work can outlive the call that started it, so no callback-scoped operation fits | `hold`, `release` |
| The idle-close decision needs to know whether accepted work is held | Yes: `holds` composes | `holds` |
| A client asks the daemon to shut down | No: one intent, one mode and one force flag are the owner's state | `shutdown` |
| A listener exit must end the daemon | No: the first fault is the owner's state | `fault` |

An operation or parameter exists only where the row says No.

## Caller example

A process part that starts work before any Session exists:

```rust
if !activity.activated().await {
    return Err(WorkError::HostClosed); // startup failed or shutdown began
}
let hold = activity.hold().ok_or(WorkError::HostClosed)?;
let result = run_work().await;
activity.release(hold);
```

The `daemon.shutdown` handler:

```rust
match activity.shutdown(ShutdownMode::Graceful) {
    Ok(()) => { /* hold the reply until the assembly settles, then write it */ }
    Err(ShutdownError::Conflict) => return Err(RpcError::invalid("Shutdown already requested")),
}
```

A part that must stop:

```rust
tokio::select! {
    _ = activity.closing() => return,
    frame = read_frame() => handle(frame).await,
}
```

## Own contract

### Dependencies

None. The owner injects no service.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `activated` | Whether the host has opened; a reload does not reset it | `{}` | `{activated}` | None | none |
| `closing` | Wait until the daemon starts closing | `{}` | `{}` when closing begins | The caller's token; the wait ends | none |
| `hold` | Keep a graceful close until the work settles | `{}` | `{hold_id}` | None; the id is settled by `release` or the row's unload | `closed` |
| `release` | The work settled | `{hold_id}` | `{released}`; an already released id is `false` | None | none |
| `holds` | Whether any accepted work is held now | `{}` | `{count}` | None | none |
| `shutdown` | Ask the process to close | `{mode: graceful \| forced}` | `{}` accepted | None; the close cannot be cancelled | `conflict` |
| `fault` | A listener exited; end the process | `{listener, message}` | `{}` | None | none |

`hold` and `release` are not a `withHold(work)` callback. The accepted work outlives the call that started it: a Workflow run spans several calls and several Sessions, and a Scheduler delivery settles after the launching call returned. No single function represents that work, and a callback form would make the owner keep the caller's future alive across those calls. The id lets the caller's business object own the hold while the owner only tracks it.

The pair is one id's acquire and release, not a multi-step session: `release` is idempotent, a lost id never corrupts owner state and only keeps a graceful close waiting, and the row's unload releases it. A hold released by the row's unload answers `false` to a later `release`. A row that crashes without unloading keeps its hold until the process ends, and `shutdown(forced)` proceeds without it.

`shutdown` does not run the sequence. It records the intent; `closing` resolves for every waiter; the assembly settles Agents, drains Rpc, closes connections and answers the initiating client. A second `shutdown` with any mode is `conflict`.

`fault` records the first listener failure; the assembly reports it as the exit reason. A later fault does not replace it.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `closed` | The daemon is closing; stop starting work |
| `conflict` | A shutdown was already requested; answer the client as already requested |

No other error exists: the owner has no "not found", "invalid input" or "too large" state.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `hold` | Increment under the admission check; a closing owner refuses | — | The row's holds are released | New holds are refused as `closed`; held work keeps the close graceful | The holder and the owner |
| `shutdown` | Record the mode and the force flag, resolve `closing` | — | — | The intent survives until the process ends | The owner |
| `closing` | Resolve every waiter once | The caller stops waiting; the intent stands | The waiter disappears | — | The owner |
| `fault` | Record the first fault, resolve `closing` | — | — | A later fault is ignored | The owner |

## Exceptions with reasons

- `processActivity` is the single published name for a merged owner: the publication gate, the holds and the shutdown intent share one plugin and one lifecycle, and `serverControl` had no method of its own. Keeping the name avoids a wire rename for Workflow and Scheduler.
- `hold`/`release` are two calls rather than `withHold(work)`: the accepted work outlives the call (a Workflow run, a Scheduler delivery), so no callback represents it. Settlement: the calling row's unload releases its holds; the owner refuses new holds once closing starts and waits for the released ones; a lost owner keeps the close graceful until the process ends, and a forced close does not wait.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | ServerControl is one process part; it writes no Timeline fact and is not a Capability. |
| §2 scopes | Process: the gate, the intent and the holds outlive every Session. |
| §3 orthogonality | No other owner's state is read. Session pauses are Agents'. |
| §4 rules 1–6, 9 | No event, handoff, before callback, business meaning, batch, model context or Timeline fact. |
| §4 rule 7 | Callers use these operations; no caller sees the gate, the intent or the hold table. |
| §4 rule 8 | No infrastructure change. |
| §7 no proxy | The owner does not hold another owner's guard; `holds` is a count, not a list of ids. |
| ADR 002 D1 | Process scope: one per process, shared by every Session. |
| ADR 002 D2 | No wiring plugin: the service is on the owner type. |
| ADR 002 D3 | No dependency; every part may depend on it. |
| ADR 002 D4 | No business setting. |
| ADR 002 D5 | The contract is data; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

- **Agents (#2020)**: `quiesce` ([#2098](https://github.com/arcships/dim/pull/2098)) — the assembly calls it after `shutdown` is accepted; the pause lives in Agents until its generation unloads, so no guard crosses to this owner.

## Binding appendix

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownMode { Graceful, Forced }

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ActivityError {
    #[error("The daemon is closing")]
    Closed,
    #[error("Shutdown already requested")]
    Conflict,
}

#[rutis::service(name = "processActivity")]
impl ProcessActivity {
    pub async fn activated(&self, request: Empty) -> ActivatedReply { /* ... */ }
    pub async fn closing(&self, request: Empty) { /* waits */ }
    pub async fn hold(&self, request: Empty) -> Result<HoldReply, ActivityError> { /* ... */ }
    pub async fn release(&self, request: ReleaseRequest) -> ReleasedReply { /* ... */ }
    pub async fn holds(&self, request: Empty) -> HoldsReply { /* ... */ }
    pub async fn shutdown(&self, request: ShutdownRequest) -> Result<(), ActivityError> { /* ... */ }
    pub async fn fault(&self, request: FaultRequest) { /* ... */ }
}
```

`HoldId` is a `u64`; the id is not authority — the owner releases it by id or by the calling row's unload.
