# PromptSuggestion

Owner: [prompt_suggestion.rs](../../../../crates/dim-agent-capabilities/src/prompt_suggestion.rs).
Offers the message the user would plausibly send next, as a Session notice. Design:
[PromptSuggestion](../../../design/plugins/PromptSuggestion.md).

## PromptSuggestion

A Session part. It observes its own Session's broadcast on `session/{sessionId}`,
folds each `main` turn's last `model/call` and `model/content`, and serves
`session.suggestion.request` and `session.suggestion.respond` through the
`session.suggestion` namespace of `sessionMethods`. A request checks its gates,
including the one that reads `main`'s `ContextPreparation::usage` and refuses a
head that would compact first, clones the turn's last request with
[`suggestion_request`](../../../../crates/dim-agent-capabilities/src/prompt_suggestion.rs),
resolves the connection, calls `ModelHost`, reads one line and validates it. The
result leaves as `session.notice` with source `PromptSuggestion`: `offer` with
`{turnId, text}`, or `cleared` with `{turnId, reason}`. No Timeline event, no
storage, no replay, no `model/usage` event.

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:SessionIdentity` | The Session's id: the broadcast channel, and the `sessionId` of the notices. |
| `S:SessionConfig` | `interactive` and `collaboration_role`: only a root interactive Session offers. |
| `S:SessionLifecycle` | `closing()` clears the offer before the channel ends. |
| `S:SessionMethods` | Registers `session.suggestion` from `apply`, until this generation unloads. |
| `S:TreeCommands` | Resolves `main` for the context gate, which reads that branch's `ContextPreparation::usage`. |
| `P:ModelHost` | Invokes the suggestion request. |

`PromptSuggestionSettingsHost`, the `prompt-suggestion` source read at each
decision, is provided by the App Server's execution-root assembly; where the
name is absent the feature is off. The local-execution profile installs the part
in the mode of its model: a process model resolves each request's connection
through `ProviderConnectionResolver`, which the part then declares and requires
as compaction does, and a local model installs it without that dependency and
sends `ModelConnection::Local` as `ModelProviderPlugin::new` does.

### Events and lifetime

No Timeline write and no Dispatcher subscription. The part listens to the
Session channel with `ctx.listen::<SessionBroadcast>()`, records `turn/start`,
`turn/end`, `model/call`, `model/content`, `input/message` and `input/applied`
on `main`, and listens to `settings/prompt-suggestion` for a setting that turns
off. One request may be in flight per Session, on the part's own `TaskTracker`;
`closing()` and the part's unload cancel it. The offer and the counters are
generation memory, never rebuilt.

Logs carry the turn id, the reason code and counts, never the text or the
conversation.

<!-- generated:plugin PromptSuggestion — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `P:ModelHost`、`S:SessionConfig`、`S:SessionIdentity`、`S:SessionLifecycle`、`S:SessionMethods`、`S:TreeCommands` |
| 提供 | — |
| 被注入于 | — |
<!-- /generated -->
