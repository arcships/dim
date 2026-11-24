# Request settings

Scope: session. Issue: #2025.

The owner publishes the model settings a turn starts from: the Session's model selection, its adopted capability snapshot, the config to send and the request metadata, as one value. Today the same need is the `prepareModelHost` object port; this document replaces it. The owner has no state of its own and writes nothing.

## Use and scope: responsibility split

The owner publishes:

- **The default settings value.** `{providerId, modelId, capabilitySnapshot?, config?, system?, metadata?}` derived once, when the Session is assembled, from the creation record and — when the profile has one — the model catalog.
- **The allowance repair.** The adopted output allowance is bounded by the model's declared output and half the window before the value is published; callers never recompute it.

Callers keep:

- Which messages the request carries: Loop's history and guide.
- Which tools it declares: ToolBinding's pinned definitions for the request.
- The turn and request ids and the session id in the metadata: Loop writes them.
- The adopted configuration published during the run: Prompt adopts sources and model changes into the Timeline as `context/configured`, and Loop reads that record instead of this value.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Loop prepares the model settings of a turn that has no adopted configuration yet | No: the catalog defaults and the allowance rule are the owner's knowledge; composing them in Loop duplicates the provider profile's decision | `settings` |
| Loop requests with a configuration Prompt adopted during the run | Yes: Loop reads the adopted record from the Timeline | Business |
| A caller changes the model for the next turn | Yes: Prompt adopts the change as the `context/configured` Timeline record; the creation record does not change | Business |

## Caller example

```rust
// Loop: the first request of a Session before any configuration is adopted.
let defaults = request_settings.settings();
let settings = ModelSettings {
    provider_id: defaults.provider_id,
    model_id: defaults.model_id,
    capability_snapshot: defaults.capability_snapshot,
    config: defaults.config,
    system: Some(guide.compose_system(&branch, defaults.system)),
    tools: Some(binding.definitions(&request_id)?),
    metadata: defaults.metadata.map(|mut metadata| {
        metadata.session_id = Some(session_id);
        metadata.turn_id = Some(turn_id);
        metadata.extensions.insert("requestId".into(), request_id.into()); // per request, written by Loop
        metadata
    }),
};
```

## Own contract

### Dependencies

None. The assembly provides the value; the aimux profile also reads its model catalog while building it. The owner reads no other owner's state.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `settings` | The Session's default request settings | `{}` | `{providerId, modelId, capabilitySnapshot?, config?, system?, metadata?}` | None | None |

### Errors

None: the value is fixed before the Session is published. A profile that cannot derive settings fails its own assembly instead of answering an error here.

### Flows

None: the operation holds no resource and waits for nothing.

## Exceptions with reasons

- `config` may already be the adopted allowance repair: the owner repairs the stored allowance against the declared limits, so a request started from this value cannot exceed the model's capacity.
- The value is fixed at creation: a model change during the run is a Timeline record (`context/configured`), and Loop reads that record; the value is never stale for its one use.
- `metadata` carries the profile's static extensions. The per-request members — session, turn and request id — are Loop's: the aimux request log reads `requestId` from the request in flight, so the value cannot carry it.

## Verification

- `settings` answers the record's model, snapshot and config; with a catalog entry, the config is the catalog default repaired to the declared allowance.
- A Session whose configuration was adopted mid-run is requested from the adopted record, not from this value.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part that publishes a value; it writes no Timeline event and folds no state. |
| §2 scopes | Session: the value derives from the Session's record. |
| §3 orthogonality | No other owner's state is read after assembly. |
| §4 rule 1 | No command event. |
| §4 rule 2 | No handover of a business object. |
| §4 rule 3 | No before callback. |
| §4 rule 4 | No business meaning in the core: the provider profile decides the catalog defaults. |
| §4 rule 5 | No batches. |
| §4 rule 6 | No context path: the value carries no messages. |
| §4 rule 7 | Loop calls the operation; no caller reads the owner's inputs. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | No Timeline fact: settings that a later turn reads are Prompt's adopted record, not this value. |
| §7 values, ids and objects | The reply is a value; no object, id or handle crosses. |
| §7 one interface | One name, `requestSettings`, on the owner type; the old `prepareModelHost` operation and object are removed. |
| §7 cross-language | The reply is data. |
| ADR 002 D1 | Session: the settings derive from the Session's creation record. |
| ADR 002 D2 | No wiring plugin: the value is on the owner's own service and the part reads the catalog directly. |
| ADR 002 D3 | The aimux profile reads its own process service while assembling; the default profile reads nothing. |
| ADR 002 D5 | The contract is data; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None.

## Supersedes

This contract replaces the `prepareModelHost` object; the delivery in #2025 carries the migration.

- `docs/design/contracts/runtime-owner-operations.md`: the `Loop → PrepareModelHost` row of "Model preparation, history and tools".
- `docs/design/service-faces.md`: §3.5's `prepareModelHost` row.

Rule D: no client wire protocol or user-visible behavior changes; the aimux request log keeps its `requestId`, now written by Loop.

## Binding appendix

```rust
use dim_agent_runtime::PublicService;
use dim_agent_types::{
    ModelCapabilitySnapshot, ModelConfig, RequestMetadata, SystemPrompt,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestSettings {
    pub provider_id: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_snapshot: Option<ModelCapabilitySnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<ModelConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemPrompt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<RequestMetadata>,
}

impl PublicService for RequestSettings {}

#[rutis::service(name = "requestSettings")]
impl RequestSettings {
    pub fn settings(&self) -> RequestSettings { /* ... */ }
}
```

`ModelSelection` and the reply's members are `dim_agent_types` data; `settings` answers the value itself. Errors: this owner has none.
