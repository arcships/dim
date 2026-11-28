# PromptSuggestion

Owner: [prompt_suggestion.rs](../../../../crates/dim-agent-capabilities/src/prompt_suggestion.rs).
Offers the message the user would plausibly send next, as a Session notice. Design:
[PromptSuggestion](../../../design/plugins/PromptSuggestion.md).

## PromptSuggestion

A Session part. It observes its own Session's broadcast on `session/{sessionId}`,
folds the turns of the line the Session's conversation is on (the default
branch, kept in step by `branch/default`), and serves
`session.suggestion.request` and `session.suggestion.respond` through the
`session.suggestion` namespace of `sessionMethods`. A request checks its gates,
including the one that reads the request's line's `ContextPreparation::usage` and refuses a
head that would compact first, clones the turn's last request with
[`suggestion_request`](../../../../crates/dim-agent-capabilities/src/prompt_suggestion.rs),
resolves the connection, calls `ModelHost`, reads one line and validates it. The
result leaves as `session.notice` with source `PromptSuggestion`: `offer` with
`{turnId, text}`, or `cleared` with `{turnId, reason}`. No Timeline event, no
storage, no replay, no `model/usage` event.

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:SessionScope` | The Session's own scope: `describe()` gives its id, restoration, configuration and owning Agent.
| `S:SessionScope` | The Session's own scope: `describe()` gives its id, restoration, configuration and owning Agent.
| `S:SessionLifecycle` | `closing()` clears the offer before the channel ends. |
| `S:SessionMethods` | Registers `session.suggestion` from `apply`, until this generation unloads. |
| `S:TreeCommands` | The line the conversation is on: read once for the admission, the context gate and the completion's publication recheck, whose `ContextPreparation::usage` decides `compaction_due`. |
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
on the default branch, clears its cached turn, offer and generation on
`branch/default`, and listens to `settings/prompt-suggestion` for a setting that
turns off. One request may be in flight per Session, on the part's own `TaskTracker`;
`closing()` and the part's unload cancel it. The offer and the counters are
generation memory, never rebuilt.

Logs carry the turn id, the reason code and counts, never the text or the
conversation.
