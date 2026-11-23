# ConfiguredModels

Scope: process. Issue: [#2019](https://github.com/arcships/dim/issues/2019).

ConfiguredModels is the model owner of the API-key profile: the model file passed with `--model-config` is its catalog, the environment variables the file names are its credentials, and the same operations [Provider](Provider.md) provides answer the same callers. This document records what the profile changes; the operations, caller intents, cancellation and error kinds are [Provider](Provider.md)'s.

## Use and scope: responsibility split

ConfiguredModels owns:

- **The model file.** Its providers and models, their endpoints, protocols, limits and `apiKeyEnv` names. The file is validated at load: both limits are required, and no modality operation is declared.
- **Selection.** The default model is the file's first model; an explicit target must be in the file. There is no workspace or global selection, no preferences and no account.
- **Credentials.** The environment variable the file names for a provider; no record and no secret store.
- **Execution.** One model request through the same driver, retry and recovery path as Provider.

Callers keep everything they keep with Provider: the Session's adopted model and configuration, the request, the output handling and the summary budget.

| Requirement (callers) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Choose a model for a Session | No: the file's default and its validation are this owner's | `resolve` |
| List the models a use can run | No: the file and its limits are this owner's | `selectable` |
| Run one model request | No: credentials, retries and streaming are the owner's | `invoke` |
| A media model's route and credential | No: the file declares no modality operation | `media_route` answers `unavailable` for every operation |
| Read a model's environment credential for one exchange (the media executor) | No: the file and the environment variable are this owner's | `credential` |
| Apply a preference patch | No: the file declares no effort or tier | `preferences` is absent in this profile; the client protocol rejects preferences |
| Admit a temporary connection | No: the file is the only connection source | `prepare_startup` is absent; the client protocol rejects startup |
| Learn where the recorder writes | Yes: read ModelLog's `recording` value | Business |

## Caller example

The Rpc entry and Agents call `providerService` exactly as in [Provider](Provider.md); the profile decides which owner answers:

```rust
let selection = provider
    .resolve(ResolveRequest { target: explicit, cwd: Some(cwd) }, cancelled.clone())
    .await?;
```

ModelProvider runs a turn through the same operation:

```rust
let outcome = provider
    .invoke(InvokeRequest { request, mode: Mode::Dialogue { chain }, consume, admit: None }, cancelled)
    .await?;
```

## Own contract

### Face and operations

Face `providerService`, marked on `ConfiguredModels`; operations `resolve`, `selectable`, `invoke`, `media_route` and `credential`, with [Provider](Provider.md)'s request and reply types. `media_route` answers `unavailable`: a model file declares no `<category>.<operation>`, so no media call resolves. `credential` returns the environment credential for one exchange; the file carries no generation and this profile has no `rejected`, so a refusal is terminal for the caller. `preferences`, `prepare_startup`, `retain_startup` and `release_startup` do not exist in this profile; the recorder reads ModelLog's `recording` value.

The file's entries declare both limits; a selection always carries them, so `resolve` and `selectable` cannot fail for an undeclared limit. `selectable(chat)` returns every entry in file order; `selectable(modality)` returns none.

### Errors

[Provider](Provider.md)'s kinds, minus `invalid_connection`, `invalid_path` and `output_limit`'s startup variants: `invalid` for a malformed request, `not_found` for a target missing from the file, `unavailable` for a media operation or a missing environment variable, `cancelled`, `closed`, `failed` and `provider_failed {retryable}`; `credential` has `not_found`, `unavailable`, `cancelled` and `closed`.

### Flows

[Provider](Provider.md)'s `resolve`, `selectable`, `invoke` and `credential` flows. The file is loaded at `apply`; a reload re-reads it. No record, selection or credential is persisted.

## Exceptions with reasons

- The bearer's boundary is [ManagedAccount](ManagedAccount.md#the-bearers-boundary); `credential` is the only operation that returns it.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One process part; no Timeline facts. |
| §2 scopes | Process: the file and its credentials are shared across the assembly. |
| §3 orthogonality | No other owner's state is read; the file is this owner's configuration. |
| §4 rules 1–6, 9 | No events, handoffs, callbacks, batches, context or Timeline state. |
| §4 rule 7 | Callers use the face; none reads the file or the environment directly. |
| §4 rule 8 | No infrastructure change. |
| §7 one change | The delivery part deletes the `ModelHost`, selection, selectable and auxiliary names in this profile together with Provider's. |
| ADR 002 D1 | Process scope. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | Depends on nothing but its file; callers depend on `providerService`. |
| ADR 002 D4 | The model file is structural configuration; there is no business state. |
| ADR 002 D5 | The face is language-neutral. |

## Requirements raised to mechanism owners

None. The model file and its validation are this owner's; no other owner's operation is needed.

## Binding appendix

`providerService` is [Provider](Provider.md)'s name; this profile registers the same name from `ConfiguredModels`. The types are shared, including `Credential` and `CredentialRequest` from [ManagedAccount](ManagedAccount.md); only the operations listed above exist.

```rust
impl PublicService for ConfiguredModels {}

#[rutis::service(name = "providerService")]
impl ConfiguredModels {
    pub async fn resolve(&self, request: ResolveRequest, cancelled: CancelledFn) -> Result<ModelSelection, ProviderError> { /* ... */ }
    pub async fn selectable(&self, request: SelectableRequest, cancelled: CancelledFn) -> Result<SelectableReply, ProviderError> { /* ... */ }
    pub async fn invoke(&self, request: InvokeRequest, cancelled: CancelledFn) -> Result<InvokeReply, ProviderError> { /* ... */ }
    pub async fn media_route(&self, request: MediaRouteRequest, cancelled: CancelledFn) -> Result<MediaRouteReply, ProviderError> { /* ... */ }
    pub async fn credential(&self, request: CredentialRequest, cancelled: CancelledFn) -> Result<Credential, ProviderError> { /* ... */ }
}
```
