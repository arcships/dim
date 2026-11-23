# AuxiliaryModel

Status: implemented (S4 native operation and named binding); the `auxiliaryModel` face is superseded by `providerService.invoke` ([Provider](../plugins/Provider.md)). Roadmap item S4 ([#53](https://github.com/arcships/dim/issues/53)).
Overview: [Appendix B.2 Provider row](../overview.md#b2-app-server-process-plugins). Scope:
[ADR 002](../../adr/002-plugin-scope.md) (process, provided by Provider and, in the
model-config profile, by ConfiguredModels).

## 1. Summary

The `auxiliaryModel` service has one async method `complete(model, request)`. It makes a
single non-streaming model call outside any Session: the owner of model selection resolves
the connection for the given model, calls the existing `ModelHost`, and returns the text.
It writes no Timeline, and credentials never leave that owner.

The rule that decides who provides it: the owner of model selection provides the
operation. In the product profile that is Provider, with its catalog and credentials. In
the model-config profile it is ConfiguredModels (the `--model-config` file), with the
configured catalog and the connection its `apiKeyEnv` names. The binding and the run are
shared capability code (`dim_agent_capabilities::auxiliary_model`), so both owners use one
implementation. The mock profile selects no model, so it does not provide the operation.

The caller decides which model to use and keeps that choice in its own business
configuration. The operation holds no per-caller policy: no purpose keys, no purpose
storage, and no new management methods.

The S4 done criterion has one consumer: SessionTitle (P4), which needs a call outside its
Session and cannot reach credentials. No new plugin, no new Timeline event, no
infrastructure change.

### 1.1 Why it is this small

The requirement comes from the feature list: "Auxiliary models: configure models for
auxiliary tasks", plus the settings page entry "Auxiliary model and reasoning effort". An
earlier overview revision turned this into a per-purpose policy held by Provider, with one
write path, and routed compaction through this service. Checked against the code, both were
more than the need:

- Configuring an auxiliary model is a business setting of the consumer that runs the task.
  Each consumer keeps its own choice; the CLI settings page shows them together by reading
  each owner. Nothing requires Provider to hold them.
- Compaction already has its own port, `ResolveContextModel` → `ContextModel`
  (`dim-agent-capabilities/src/host.rs`): context window, output budget, token counting and
  `compact`. A text-only `complete` would lose that information. The production compaction
  host (F1) implements that port; it does not use `auxiliaryModel`.

## 2. Structure

```
 Process scope
 +----------------------------------------------------------------+
 |  Provider (product) or ConfiguredModels (model-config)         |
 |   catalog, credentials and default selection (unchanged)       |
 |   AuxiliaryModel --uses--> shared auxiliary_model::complete,   |
 |                            Aimux ModelHost                     |
 +----------------------------------------------^-----------------+
                                                | complete(model, request)
 Session scope                                  |
   SessionTitle (Deps: auxiliaryModel) -----'
     model = its Settings source `session-title`.model, else the Session's model
```

Dependencies point to the core: both owners gain a hard dependency on Aimux's `ModelHost`
and use the shared run in `dim-agent-capabilities`; neither names a consumer.

## 3. Data and state

None. Provider stores nothing new and caches nothing; its existing admission and task
tracking own an in-flight call. ConfiguredModels has no admission and tracks no task.

## 4. Interfaces

The `auxiliaryModel` face is deleted. See [Provider](../plugins/Provider.md) for `providerService.invoke`; the headings below stay because source comments link them.

### 4.1 Language-neutral (ADR 002 D5)

Superseded: there is no `auxiliaryModel` service. A caller runs `providerService.invoke(request, Mode::Once, consume, None, cancelled)` and collects the text in its consumer.

### 4.2 Rust binding

Superseded: `AuxiliaryModel`, `AuxiliaryRequest`, `AuxiliaryReply` and `CompleteAuxiliary` are deleted. The request is a `ModelRequest`; the reply is the caller's collected text and stop reason.

### 4.3 Named binding

Superseded: the `auxiliaryModel` name leaves the catalog. The replacement operations are in [Provider](../plugins/Provider.md); the account credential they use is in [ManagedAccount](../plugins/ManagedAccount.md).

## 5. Flow

```
 Consumer          AuxiliaryModel (owner)     catalog/connection    ModelHost (Aimux)
    | complete(model, req)  |                       |                       |
    |---------------------->| check request         |                       |
    |                       | resolve model and defaults ->| selectable?    |
    |                       |<-- model config | model_unavailable          |
    |                       | build ModelRequest (system, messages, max_tokens, no tools)
    |                       | resolve connection (credentials stay here)    |
    |                       | invoke(request, connection, cancel) --------->|
    |                       |<------------ chunks: Text, Usage, MessageStop |
    |<-- {text, stopReason, usage}                                          |
```

Provider resolves the model through `ProviderService` and the connection through
`ProductResolver`; ConfiguredModels resolves both from the `--model-config` file and the
environment variable its provider names.

Failures go through the same policy as Session requests, in one shared function
(`dim-agent-capabilities::model_provider::invoke_with_recovery`, also used by
ModelProvider), so the policy has one implementation:

- **Credentials:** a credential that is rejected is recovered once. If the recovered
  credential is rejected again (`Terminal`), the credential owner's `finish_rejected`
  settles it before the call fails; for example, a managed account is signed out.
- **Transient failures:** these back off within the shared budget (at most eight attempts,
  300 s of waiting, with a malformed response retried once), unless the provider's
  `ModelFailurePolicy::forbids_retry` forbids it.

Beyond that, retry is the consumer's (SessionTitle retries on a later input). No event is
written anywhere.

## 6. Errors

| Code | When | Retry |
| --- | --- | --- |
| `invalid_request` | `messages` empty, `maxOutputTokens = 0` | No (caller defect) |
| `model_unavailable` | The model is not selectable, or its Provider or catalog is missing | After the user fixes the consumer's setting or the catalog |
| `failed` | Connection resolution, transport, HTTP or stream error after the shared policy; a tool stop | Consumer's choice |
| `cancelled` | Call dropped, cancel token fired, or Provider unloading (its stop token) | Consumer's choice |

## 7. Security

- Credentials stay in the owner: `ResolvedModelConnection` is built and used inside
  `complete`, never returned or serialized. Consumers see text, stop reason and usage only.
- Error messages carry no secrets: `failed` uses the existing `HostError` message, which
  carries no credential values; an owner never formats a connection into a message.
- Process log: `complete` records one `tracing` event, target `auxiliary.complete`, with provider
  id, model id, outcome, duration and token usage, never request or reply content. Consumers
  log their own outcomes. Auxiliary calls are not Session facts and enter no Timeline.

## 8. Relationship to existing parts

| Part | Relationship |
| --- | --- |
| Default selection (`provider.select`) | Unchanged. `complete` does not consult it |
| SessionTitle (P4) | The Rust part (`dim-session-title`) requires `AuxiliaryModel` and calls `complete`; `model` is the optional `model` of its Settings source `session-title`, else the model of the latest `context/configured` in the prefix. Without an owner that provides the operation, its required dependency keeps the part Pending; native Sessions do not depend on that row. The named `auxiliaryModel` HostDispatch binding stays for TypeScript Session parts. SessionTitle's own Ready decisions and implementation remain separate |
| Compaction host (F1) | Does not use this service. Implements `ResolveContextModel`; how it picks the model and reads the context window is F1's design |
| Collaboration | No change from S4. A member's model is its profile's model, else the default selection; a separate member default, if ever wanted, is Collaboration's own setting |
| Modality | Keeps its default model per category in its own `modality` Settings source and passes the model explicitly. Non-conversational connection resolution is a separate Provider supplement, not S4 |
| Workflow | `stage.model` > profile model > the Agents default already covers model choice; the operation has no stage default |
| `--model-config` profile (ConfiguredModels, no Provider plugin) | Provides `auxiliaryModel` from the configured catalog and the connection its provider names, through the same shared run; SessionTitle's required dependency resolves in this profile too |

## 9. Rule check

| Rule | Answer |
| --- | --- |
| Principles §3 (dependency direction) | Each owner (core) depends only on capability code and Aimux (core); consumers depend on the operation; no owner names a consumer. |
| §4 rule 3 (before callback) | Not used. |
| §4 rule 8 (infrastructure unchanged) | No change to Timeline, Dispatcher, event format, rutis or assembly; uses `ModelHost`, `ProductResolver` and the existing catalog. No requirement on rutis. |
| §4 rule 9 (only needed facts in Timeline) | Auxiliary calls write no event. |
| ADR 002 D2 (no wiring modules) | No new plugin; the shared run is capability code and the existing model-selection owners provide it. |
| ADR 002 D3 (dependencies) | No core part hard-depends on an optional plugin; SessionTitle (optional) may depend on this service. |
| ADR 002 D4 (configuration) | The operation adds no configuration; each consumer's model choice is its own business configuration. |
| ADR 002 D5 (language-neutral contracts) | §4.1 is the contract; §4.2 is one binding. |

## 10. Open questions

Deferred: per-purpose reasoning effort (feature list: "Auxiliary model and reasoning
effort"). If it is added, it is a field of the consumer's own setting and `complete`
takes it as a request parameter. It is decided with the `/settings` design, not in S4.

## 11. Acceptance (flow tests)

1. `complete` against a mock HTTP provider sends the request to the given model with the
   system text, messages, `max_tokens` and no tools; the reply is text plus usage.
2. A disabled or unknown model: `model_unavailable`, no request sent, no fallback.
3. Deleting the model's Provider: the next call is `model_unavailable`.
4. Dropping the call mid-stream cancels the request; unloading Provider ends an in-flight
   call with `cancelled`.
5. A managed account whose recovered credential is rejected again is settled (signed out)
   and the call fails with `failed`; the endpoint received exactly two requests.
6. No API key appears in the reply, errors or the `auxiliary.complete` log event; the Session's
   Timeline has no new event after a call.
7. A real Node Session part injects the process service by name and makes the HTTP call through
   `complete({model, request})`; reply fields and thrown error codes cross unchanged. Invalid
   requests and unknown models send no HTTP request, and no call writes Session history. With
   a model request held at the HTTP endpoint, closing its caller resolves the cancellation
   reference and closes the HTTP stream while Provider remains loaded. Without an
   auxiliary-model owner, native Session creation still succeeds and only the foreign
   namespace remains unavailable.
8. The model-config profile provides and runs the operation from its own catalog and the
   connection its provider names: a call sends the request to the configured model with
   `maxTokens` clamped to the model's adopted output allowance (derived from the file's
   declared pair); an undeclared model is `model_unavailable` with no request sent; a Session
   part and a Session's automatic title both reach it in that profile.
