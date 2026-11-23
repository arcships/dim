# ProcessRoot

Scope: process. Issue: #2021.

The process root is the assembly's provider of process-scope values. This document is the owner's target contract for the three values the root provides, `reservedMethods`, `credentialVariablesHost` and the daemon's peer id. The current implementation is in `crates/dim-app-server/src/lib.rs`. The consuming owners — the Session method table and Mcp — are separate owners.

## Use and scope: responsibility split

The root owns:

- **The Session name reservation policy.** Which namespaces and built-in method names the host keeps for itself, consulted before a Session table registers a namespace.
- **The daemon's credential variable names.** The environment variable names the daemon's model configuration reads credentials from; the names only, never values.
- **The daemon's peer id value.** The `rutis_bridge::LocalId` the bridge reads when it greets a peer.

The root holds no resource that needs settling: all three are values fixed at assembly.

Callers keep:

- What a reserved name means: the Session method table refuses a registration; the root only answers.
- What a credential variable is used for: Mcp decides where a name may not be expanded.
- The bridge handshake: the root provides the value; rutis-bridge reads it.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A Session table must know the host's reserved names before registering | No: the policy is the host's, and a caller that guessed it would duplicate the protocol's own list | `reservedMethods.check` |
| Mcp must not expand a credential variable into a third-party configuration | No: only the daemon knows which names its model configuration reads | `credentialVariablesHost.variables` |
| A peer needs the daemon's id | Yes: rutis-bridge reads the `LocalId` value; no service call is involved | value |

An operation or parameter exists only where the row says No.

## Caller example

The Session method table consults the policy when it registers a namespace:

```rust
// Registered by the part that owns the namespace; refused names fail the registration.
table.register(RegisterRequest { namespace: "session.goal".into(), declarations, call }).await?;
// The table's own check calls reservedMethods.check with the same declarations.
```

Mcp reads the names once per catalog build:

```rust
let protected = credentials.variables().await?.variables; // names only
if protected.contains(name) {
    return String::new(); // a guarded field never expands a host credential
}
```

## Own contract

### Dependencies

None. The root reads the assembly's configuration and provides the values.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `check` | Refuse a namespace or a declaration the host keeps | `{namespace, declarations}` | `{}` | None; the owner completes it | `reserved` |
| `variables` | The daemon's credential variable names | `{}` | `{variables}` | None | none |

`check` is synchronous with the caller's registration: a table that proceeds without it would accept a namespace the protocol cannot route. The policy is a callback by reference the assembly binds; the check itself is data in and out.

`localId` has no operation. The peer id is a value (`rutis_bridge::LocalId`); the data face `localId.get` had no caller and is retired.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `reserved` | The namespace or a name is the host's; choose another name |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `check` | Read the policy value and answer | None | — | The value is fixed for the process generation; a reload provides the same policy | Root |
| `variables` | Read the configuration's names | — | — | Same value across generations | Root |

## Exceptions with reasons

- The root has no plugin row: the assembly is the provider, and a process-scope value needs no `apply`. It provides no resource, so no settlement path exists.
- `credentialVariablesHost` covers the configured model catalog's names only; the managed provider's credentials are not environment variables and are not listed.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | The root is the assembly, not a part; it writes no Timeline fact and is not a Capability. |
| §2 scopes | Process: the values are read by process and Session parts. |
| §3 orthogonality | No other owner's state is read. |
| §4 rules 1–6, 9 | No event, handoff, before callback, business meaning, batch, model context or Timeline fact. |
| §4 rule 7 | Callers use the two operations; no caller sees the configuration. |
| §7 no proxy | Each value is the owner's own; no facade stands between. |
| ADR 002 D1 | Process scope: values read at assembly. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | Dependencies point to the root; it depends on nothing. |
| ADR 002 D4 | The values are structural configuration. |
| ADR 002 D5 | The contract is data; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None.

## Binding appendix

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckRequest {
    pub namespace: String,
    #[serde(default)]
    pub declarations: Vec<ReservedMethod>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReservedMethod {
    pub name: String,
    pub mutating: bool,
    #[serde(default)]
    pub notifications: Vec<String>,
}

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ReservedError {
    #[error("{message}")]
    Reserved { message: String },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VariablesReply { pub variables: std::collections::BTreeSet<String> }

#[rutis::service(name = "reservedMethods")]
impl ReservedMethods {
    pub async fn check(&self, request: CheckRequest) -> Result<(), ReservedError> { /* ... */ }
}

#[rutis::service(name = "credentialVariablesHost")]
impl CredentialVariablesHost {
    pub async fn variables(&self, request: Empty) -> VariablesReply { /* ... */ }
}
```

The Session table's Rust binding calls the same `check` through the policy value the assembly binds; no second path exists.
