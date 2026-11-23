# EventHub

Scope: process and Session. Issue: [#2023](https://github.com/arcships/dim/issues/2023).

EventHub owns the catalog of event sources, the subscriptions, the delivery of one event to the branches that want it, and the Session-side `events` entry. This document is the owner's target contract; the current implementation is in `crates/dim-event-hub`, and the scan report and delivery comment are on #2023.

## Use and scope: responsibility split

EventHub owns:

- **The catalog.** Every declared source, its guide and its types, tied to the declaring row; a source exists exactly while that row is loaded.
- **Subscriptions.** The durable record per branch and source pattern in `eventhub/subscriptions.json`, its guide records and digests, and the limits of 32 per Session and 1024 in total.
- **Delivery.** One input per event per branch, the receipt per target, the guide refresh before an event, `lastFailure`, and the shared delivery budget.
- **The deletion reconciliation.** A deleted Session's subscriptions are removed; a closed, restorable Session keeps them.
- **The Session entry.** The `events` tool and the guide entry for `dim-event` and `dim-event-guide`.

Callers keep:

- What an event means, when to publish it, and the source's guide text.
- Which Session and branch ids to target, and the subscription's types, `match` and `mode`.
- The model-facing policy of the `events` tool's description; the owner supplies the shipped text.
- Permission and approval of the tool calls.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A loaded producer row declares its source | No: the catalog is the owner's and the declaration ends with the row | `declare` |
| A producer publishes one event | No: the event id's per-branch idempotency, the guide refresh, the budget and the receipt are the owner's | `publish` |
| A branch subscribes to types | No: the guide input and the record must commit in one order under the Session's lock | `subscribe` |
| A branch removes one subscription | No: the record removal and the guide retire are one order | `unsubscribe` |
| A caller reads a Session's subscriptions or every subscription | No: the records are the owner's | `list` |
| A caller reads the declared catalog | No: the catalog is the owner's | `sources` |
| A source's guide changes | No: the digest and the refresh at the next delivery are the owner's | inside `publish` |
| A subscribed Session is deleted | No: the notice and the reconciliation are the owner's | internal |
| The model manages its own Session's subscriptions | No: the tool maps to the same operations with its own Session | the `events` tool |
| The scenario-eval harness manages a Session's subscriptions | No: the management namespace is removed; the harness reads the process service | `subscribe`, `list` |

An operation or parameter exists only where its row says No.

## Caller example

A Rust producer row declares its source and publishes one event. `&ctx` is the binding's caller parameter, not a request field.

```rust
hub.declare(
    &ctx,
    DeclareRequest {
        source: "im".into(),
        guide: guide_text(),
        types: vec![TypeDeclaration { r#type: "im.message".into(), description: "...".into(), mode: InputMode::Queue }],
    },
)
.await?;
let receipt = hub
    .publish(
        &ctx,
        PublishRequest {
            source: "im".into(),
            r#type: "im.message".into(),
            id: Some("7f3a".into()),
            title: Some("Build failed".into()),
            body: body_text,
            attributes: attributes(),
            to: vec![ToTarget { session_id, branch_id: None, mode: Some(InputMode::Steer) }],
        },
    )
    .await?;
// The receipt is the outcome; a producer that must not wait does not await it.
for delivery in receipt.deliveries {
    match delivery {
        Delivery::Delivered { .. } => {}
        Delivery::Gone { .. } | Delivery::Failed { .. } => { /* the producer records the outcome */ }
    }
}
```

The `events` tool manages its own Session. The branch defaults to the calling tool context.

```rust
let created = hub
    .subscribe(SubscribeRequest {
        target: SubscribeTarget { session_id: identity.session_id.clone(), branch_id: Some(branch) },
        types: vec!["im.*".into()],
        matches: None,
        mode: Some(InputMode::Steer),
        origin: Origin::Tool,
    })
    .await?;
let own = hub.list(ListRequest { session_id: Some(identity.session_id.clone()) });
```

The scenario-eval harness reads the process service directly; it no longer uses a management namespace.

```rust
let hub = server.service::<EventHub>();
hub.subscribe(SubscribeRequest {
    target: SubscribeTarget { session_id: session.clone(), branch_id: Some("main".into()) },
    types: vec!["im.*".into()],
    matches: None,
    mode: None,
    origin: Origin::Client,
})
.await?;
let all = hub.list(ListRequest { session_id: None });
```

## Own contract

### Dependencies

- `sessions` ([#2020](https://github.com/arcships/dim/issues/2020)): `input` for an event and a guide, `lookup` for the per-branch idempotency, `exists` for the deletion reconciliation, and `defaultBranchId` for a request that omits its branch. `agent/session-deleted` is the notice that removed the Session's stored rows.
- `sessionFiles` ([#2018](https://github.com/arcships/dim/issues/2018)): `put` for a body over 5120 bytes; the reply must name what the read tool opens.
- `pluginFiles` ([#2016](https://github.com/arcships/dim/issues/2016)): the by-name text update and read for `eventhub/subscriptions.json`, under the file's lock.
- `processActivity` ([#2021](https://github.com/arcships/dim/issues/2021)): `hold` and `release` for accepted publish and subscription work, and `activated` before the reconciliation reads the Sessions.
- `guideEntries`, `toolRegistry` and `partRegistry`: the registration points the Session part uses; they add no operation.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `declare` | Make a source addressable under the calling row | `{source, guide, types}` | `{}` | None; the owner completes the in-memory insert | `duplicate_source`, `invalid_declaration` |
| `publish` | Deliver one event to every matching branch and explicit target | `{source, type, id?, title?, body, attributes?, to?}` | `{eventId, deliveries}` | None; accepted work completes | `unknown_source`, `unknown_type`, `too_large`, `invalid_event` |
| `sources` | The declared catalog as stored now | `{}` | `{sources}` | None | none |
| `subscribe` | Create one subscription and write the source's guide to the branch | `{target, types, match?, mode?, origin}` | `{subscriptionId}` | Before the guide write; the record is stored only after the guide commits | `unknown_type`, `limit_reached`, `session_closed`, `branch_missing`, `branch_ended`, `reloading`, `rejected`, `ambiguous`, `failed`, `publication_unknown`, `transport`, `storage` |
| `unsubscribe` | Remove one subscription and retire the guide when no remaining subscription names the source | `{subscriptionId}` | `{}` | Before the record removal | `subscription_unknown`, `storage` |
| `list` | The subscriptions of one Session, or every subscription | `{sessionId?}` | `{subscriptions}` | None | none |

`origin` is data: `tool` for the `events` tool and `client` for a client or the harness. The tool always supplies its own Session id; the branch defaults to its calling branch. `publish` takes the target branch ids as data and never a Session handle. `to` is 0 to 16 targets; zero targets reply `deliveries: []`.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `duplicate_source` | Another row holds the source; pick another source or stop declaring |
| `invalid_declaration` | Fix the source, guide or type bounds |
| `unknown_source` | The calling row holds no such source; declare it first or publish another source |
| `unknown_type` | The source does not declare the type now; fix the type or declare it |
| `too_large` / `invalid_event` | Fix the body or fields; the event was not delivered |
| `unknown_type` (subscribe) | Fix the pattern or declare the source |
| `limit_reached` | Remove a subscription first; 32 per Session, 1024 in total |
| `session_closed` / `branch_missing` / `branch_ended` | The target cannot take input; fix the target |
| `reloading` / `rejected` / `ambiguous` / `failed` / `publication_unknown` / `transport` | The `sessions` call's answer; the subscription is stored only after a committed guide, `publication_unknown` included |
| `storage` | The subscription file was not written; the in-memory set is unchanged |
| `subscription_unknown` | No such subscription of the calling Session; read `list` first |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `declare` | Check the source and types, insert under the calling row, register the withdrawal | The call is in-memory and completes | The row's declaration is withdrawn | The catalog survives a reapply of the same install; a new install starts empty | The declaring row |
| `publish` | Take a hold and the event lock, check the source and type, resolve targets and omitted branches, look up each branch, store one overflow file per Session, write one input per branch, refresh stale guides, write the subscription changes in one transaction, answer the receipt | Accepted work runs to completion; a dropped reply does not stop it | The producer row may unload; the publish uses the source snapshot it started with | In-flight publishes finish in the owner's tasks; a new generation reads the subscription file again | The owner for the inputs and file transaction; the target branch for the input |
| `subscribe` | Check types and limits, resolve an omitted branch, take the Session lock, write the guide input, store the record and the guide digest in one transaction | Before the guide write: nothing stored. A refusal after a guide write retires the guides no stored subscription names | The call ends; the guide write already admitted stands | In-flight calls settle; a new generation reads the file | The owner for the record; the target branch for the guide input |
| `unsubscribe` | Take the Session lock, remove the record, retire the guide of each source no remaining subscription names | Before the removal: nothing changes | The call ends | In-flight calls settle | The owner |
| `list` / `sources` | Read the in-memory set or catalog | Nothing to cancel | Nothing | The new generation reads the file | The owner |
| Session delete | The `agent/session-deleted` notice removes every subscription whose target is that Session, under its lock | — | — | A missed notice is repaired by the reconciliation at the next apply or the first failed delivery | The owner |

### 9.1 Agents announces every delete that removed the stored rows

EventHub must distinguish a deleted Session from a close that keeps its rows. `session_closed` answers both, so EventHub consumes the host-internal `agent/session-deleted {agentId, sessionId}` notice, emitted after the Session's stored rows are removed, loaded or unloaded. A missed notice is repaired by `sessions.exists` at the next apply or after a delivery fails `session_closed`; a closed, restorable Session answers true and keeps its subscriptions.

## Exceptions with reasons

- `eventhub/ready` is a bus notice, not an operation: a producer that applied before EventHub's first apply cannot read the service yet, and the notice tells it to declare. A missed notice only delays the declaration; `unknown_source` recovers it.
- The subscription file is the owner's own file under `pluginFiles`; the owner keeps its schema and version.

## Verification

Flow tests on the owner:

- A producer declares and publishes; a subscribed branch receives one input with the `dim-event` marker; the receipt names the branch and input.
- A republish with the same id writes nothing and answers the first input; a branch reached by `to` and a subscription receives one input.
- A delivery to a deleted Session is `failed: session_closed` and keeps the subscription; a `gone` delivery removes each listed subscription.
- A body over 5120 bytes stores one Session file per target Session; the marker carries the value the read tool opens.
- Subscribe writes the guide; the last unsubscribe retires it; a failed subscription-file write retires the guide it wrote.
- Deleting a subscribed Session removes its subscriptions; a closed, restorable Session keeps them.
- The `events` tool scopes every call to its own Session; another Session's subscription id is `subscription_unknown`.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | EventHub is one process part and one Session part. It writes no Timeline fact and folds no state, so it is not a Capability. |
| §2 scopes | The catalog, subscriptions and delivery are process state; the `events` tool and the guide entry are Session state. |
| §3 orthogonality | EventHub reads no other plugin's state; it calls `sessions`, `sessionFiles` and `pluginFiles` operations and registers its own entries. |
| §4 rule 1 | EventHub writes inputs through `sessions.input`; InputQueue and Loop decide what an input becomes. |
| §4 rule 2 | A delivered input belongs to InputQueue; EventHub cannot withdraw it and keeps no copy. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | The Timeline and the core carry no event meaning; the marker is the owner's text. |
| §4 rule 5 | No Timeline batch. |
| §4 rule 6 | An event reaches the model only as an input or a tool result; the guide entry is Prompt's route. |
| §4 rule 7 | The dependencies are the operations above; no caller reads the owner's state. |
| §4 rule 8 | The Agents notice is the mechanism requirement in §9.1; the rest is the owner's. |
| §4 rule 9 | No Timeline event; subscriptions, the catalog and the failure record are the owner's own state. |
| §7.2 ids, one face | The face is `eventHub`; `declare`/`publish` take the binding's `caller: &Ctx`, not a request field. The operations cross by data; a Session, branch, subscription or event is an id or value. |
| §7.2 no proxy | The `events` tool is a registration point, not a facade: it maps the model's actions to the owner's operations. |
| ADR 002 D1 | Process for the catalog, subscriptions and delivery; Session for the entry and tool. |
| ADR 002 D2 | No wiring plugin: the service is on the `EventHub` type; the `events` tool and guide entry are the owner's own Session part. |
| ADR 002 D3 | EventHub depends on core process services; no core part depends on EventHub. |
| ADR 002 D4 | The row's enablement is PluginManager's; subscriptions are business state in the owner's file. |
| ADR 002 D5 | The contract is data and ids; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

- **Agents (#2020):** the `sessions` operations `input`, `lookup`, `exists` and `defaultBranchId` as data operations with the errors above, and the `agent/session-deleted {agentId, sessionId}` notice. EventHub does not take a `HostDispatch` object or decode the reply envelope.
- **SessionFiles (#2018):** `put` answers the value the read tool opens for a Session file — a path, or a file id the read tool accepts. EventHub carries that value in the `dim-event` marker; `{fileId, size}` alone does not let the model read the body.
- **PluginFiles (#2016):** the by-name text update and read for the owner's own file, with the lock held across the caller's change; no scope object.
- **ServerControl (#2021):** `hold`/`release` ids for accepted publish and subscribe work, and `activated` for the reconciliation. EventHub does not hold a `ProcessHold` guard.

## Binding appendix

The service is marked on the `EventHub` owner type; the wire name stays `eventHub`. `caller: &Ctx` is rutis's caller parameter: the dispatch fills it from the calling row, it is not a request field, and no caller sends or names a row. The other operations take only request data.

```rust
use dim_agent_runtime::PublicService;
use rutis::host::BoxFuture;
use rutis::Ctx;
use std::sync::Arc;

pub type SubscriptionId = String;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclareRequest {
    pub source: String,
    pub guide: String,
    pub types: Vec<TypeDeclaration>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishRequest {
    pub source: String,
    pub r#type: String,
    pub id: Option<String>,
    pub title: Option<String>,
    pub body: String,
    pub attributes: std::collections::BTreeMap<String, String>,
    pub to: Vec<ToTarget>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeRequest {
    pub target: SubscribeTarget,
    pub types: Vec<String>,
    #[serde(rename = "match")]
    pub matches: Option<std::collections::BTreeMap<String, String>>,
    pub mode: Option<dim_agent_types::InputMode>,
    pub origin: Origin,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Tool,
    Client,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum Delivery {
    Delivered { session_id: String, branch_id: String, subscription_ids: Vec<String>, input_id: String, sequence: u64, truncated: Option<bool>, repeated: Option<bool> },
    Gone { session_id: String, branch_id: String, subscription_ids: Vec<String>, code: String },
    Failed { session_id: String, branch_id: Option<String>, subscription_ids: Vec<String>, code: String, message: String },
}

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct EventHubError {
    pub code: &'static str,
    pub message: String,
}

#[rutis::service]
impl EventHub {
    pub async fn declare(&self, caller: &Ctx, request: DeclareRequest) -> Result<(), EventHubError>;
    pub async fn publish(&self, caller: &Ctx, request: PublishRequest) -> Result<Receipt, EventHubError>;
    pub fn sources(&self) -> Sources;
    pub async fn subscribe(&self, request: SubscribeRequest) -> Result<SubscribeReply, EventHubError>;
    pub async fn unsubscribe(&self, request: UnsubscribeRequest) -> Result<(), EventHubError>;
    pub fn list(&self, request: ListRequest) -> Subscriptions;
}
```

The `events` tool registers with `toolRegistry` and reads this same service; the guide entry registers with `guideEntries`.
