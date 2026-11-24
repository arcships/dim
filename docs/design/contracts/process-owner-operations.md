# Process owner operations

The target removes `sessionPermissions`, `selectableModelsHost`, `modelSelectionHost` and `auxiliaryModel` from the catalog when their consumers move to the owners below.

## Retained operations

| Caller → owner | API to use |
| --- | --- |
| Model selection, pickers and auxiliary callers → `providerService` | `resolve`, `selectableModels`, `selectableForModality`, `complete` keep their existing arguments and replies. Provider supplies these operations in the managed profile and ConfiguredModels in the API-key profile. |
| Media selection → Modality | `modalityTargets.resolve`. |
| Management clients → Provider / Grok OAuth / OpenAI Codex OAuth | Existing [Provider management RPCs](../appserver/provider-management.md) and account status, begin, attempt status, cancel and logout RPCs. Codex also handles its browser callback. |
| Session consumers → Agents | [`sessions` operations](https://github.com/arcships/dim/pull/2098/files). Agent administration and Session creation/fork retain the existing `agent.create/list/close` and `session.create/fork` client RPCs. RPC, Agent configuration, Collaboration and Provider lifecycle consumers use Agents' admission, leases, `observe`, close and rollback. A lease is row-owned and kept across calls ([ADR 027](https://github.com/arcships/dim/pull/2029/files) D5). |
| Plugin management → loader | `rutis/loader.create/reload/remove/status`. The loader owns native profile construction and group instantiation with the original parent and group. |
| Presence consumers → Connection | `sessionClientsHost.presences`. Connection serves this operation directly and handles broadcast, socket registration, writes, close and detach. |
| Activity consumers / daemon clients → ServerControl | `processActivity.activated/hold/release`; existing `daemon.ping/shutdown` RPCs. Shutdown settles the initiating reply before transport teardown. |
| UI clients → UiHost / UiExtensions | Existing `ui.open/attach/replace/status/shutdown/close` RPCs and [extension listing, resources and notices](../client-ui-extensions.md). Transport supplies the authenticated connection identity. UiHost owns attach tokens, generations and disconnect cleanup. |
| Prompt, scenario and summary evaluation tools → active profile | Use `providerService` for their catalog/selection calls and `modelHost.invoke` for execution. |
| Media execution and realtime → native adapter | Existing native connection binding and credential owner ([#1613](https://github.com/arcships/dim/issues/1613), [#1634](https://github.com/arcships/dim/issues/1634)). |
| Transport, Web entry and Session subscriptions → Connection | Connection-owned attach, broadcast, close and detach using the authenticated connection identity. |

## New rutis bindings

| Caller → owner | Methods and replies |
| --- | --- |
| Permission consumers → Permission and Agents | A line's current policy is Permission's published value, read by `(sessionId, branchId)` id ([ADR 027](https://github.com/arcships/dim/pull/2029/files) D1); there is no `sessions.permission`. `sessions.permissionDefaults` reads the current creation default without creating a Session. |
| Dialogue, summary and suggestion → active invocation owner | `modelHost.invoke(request, mode, consume, cancelled, hooks?) → value`. Full `ModelRequest` with pinned tools, configuration, cached prefix and capability snapshot. Attempt contract below. |
| Session configuration → `providerService` (managed profile) | `configuration(explicit?, cwd?, preferences) → ModelSelection`; `applyPreferences(current, update) → ModelSelection`. Bind the existing Provider operations. |
| Session creation/adoption and Collaboration → `providerService` (managed profile) | `prepareStartup(request) → {model, admissionId?}`, `checkStartup(source) → void`, `retainStartup({sessionId, generation, model, admissionId?}) → void`, `releaseStartup({admissionId}) → void`. Reuse the pending startup binding ID. Retain on success for that Session generation; release on failure or caller unload. |
| Session creation / worktree management → ManagedWorktrees | `create({source, base?, branch?}) → CreatedWorktreeReply`, `discard(created) → void`, `list({}) → WorktreeView[]`, `pruneSelection({olderThanMs?, keep?, force}) → PruneSelection`, `remove({taskId, force}) → boolean`. Bind the existing operations. Failed Session creation discards the created worktree on its node. |
| PluginManager → UiExtensions | `plugins({}) → string[]`, `refusal({plugin}) → string or null`, `register({plugin, name, package?}) → void`, `withdraw({plugin}) → void`. Bind the existing catalog methods. |

### Session use and generation observation

| Caller → owner | API and lifetime |
| --- | --- |
| Collaboration → Agents | `acquire({sessionId, agentId?}) → {leaseId, generation}` runs Agents' admission and verifies the Agent before returning; `generation` is the mounted instance. Agents keeps the lease until `release({leaseId})` or the caller row unloads. One lease covers delivery through request settlement, including a delayed stop and idle; failed admission releases it. |
| Collaboration → the line's owners | The line's input, pending, stop and published values are called by `(sessionId, line)` id ([ADR 027](https://github.com/arcships/dim/pull/2029/files) D1); no request carries a lease, and the lease only keeps the generation loaded. An expired lease or mismatched target fails without restoring another generation. |
| Collaboration and Provider → Agents | `observe({sessionId, generation?}, onEnd) → {agentId, generation} or null`, bound to the matching loaded instance; no restore or idle hold. Deliver `onEnd({sessionId, agentId, generation, reason})` once, with reason `unloaded` or `closed`. The observation is owned by the caller row; stale completions cannot remove a newer watch. |
| Agents → Collaboration and Provider | Keep `agent/session-restored` (with the mounted generation), `agent/unloaded-session-deleted` and `agent/session-deleted`. A business close or deletion ends relations and startup ownership; idle unload preserves them. |
| Provider → the line's owners | Provider reads each line's published adopted startup connection by `(sessionId, line)` id ([ADR 027](https://github.com/arcships/dim/pull/2029/files) D1); Agents provides no proxy. Omit lines without an adoption. No restore or credential read. Retain uses the current target and matching destinations; restore reattaches ownership, pinned to the generation supplied by its creating/adopting caller. Register observation before collecting sources; apply results only while that generation remains current. |

### Connection settlement

| Caller → owner | API and completion |
| --- | --- |
| Client → Connection | Existing Session detach RPC retires that Session attachment under its transition lock, calls UI detach, publishes presence and joins the attachment before replying. |
| Connection → UiHost | Bind existing `detach({connectionId,sessionId})` on detach and `release({connectionId})`, then `finished({connectionId})` on connection finish. These use the transport's connection identity. |
| Local transport / Web socket → Connection | Socket EOF, entry shutdown and failed handshake enter the same connection finish path: cancel calls, release local Web holder if present, settle UI, retire attachments, remove the connection. Web entry owns its holder set, cancellation and listener join. |
| Host shutdown → Connection | Existing hub close signal ends readers and writers and joins their tasks. Client `daemon.shutdown` settles its initiating reply before teardown. Client detach and entry disable retain their narrower scopes. |

### Model invocation

| Input | Contract |
| --- | --- |
| `mode` | `once` for suggestions; `recovering` accepts an optional existing FailureChain. Dialogue supplies the replay decision. |
| `consume(next, cancelled)` | `next` yields one chunk, end or typed host error. Return `{kind: returned, value, replaySafe}` or `{kind: failed, error, replaySafe}`. The reader expires when the consumer settles. Accepted output prevents unsafe replay. |
| `hooks?` | `prepare(protocol or null) → ModelConfig or null` runs once after resolution. `beforeAttempt() → admitted or {rejected: message}` checks the summary budget before each attempt. `waiting(progress or null)` reports the existing retry schedule. |
| Completion | Cancellation or driver shutdown releases a suspended consumer and joins accepted work. Invalid wire input returns `invalid_request`. Consumer exceptions remain consumer errors. |

The full-request consumer injects only `modelHost`. The managed profile registers Provider at that name; the API-key profile registers ConfiguredModels; standalone local configuration registers the local host. Each profile registers exactly one dispatcher at `modelHost`. Aimux relinquishes the catalog name and supplies the existing native driver. The selected owner resolves the request, invokes its driver, and settles credential recovery with the account owner. The local host uses its local connection. The caller receives output or a host error. Aimux owns its native driver, pool and unload signal.
