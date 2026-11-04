# EnvironmentSettings

Status: Draft. Parent: [agent environment awareness](../agent-environment.md). Issue: [#1020](https://github.com/arcships/dim/issues/1020). Rules: [design rules](../../architecture/rust-agent/principles.md) §1, §4 and §5, [ADR 002](../../adr/002-plugin-scope.md), [model-facing prompts](../model-prompts.md). Language: Rust.

EnvironmentSettings states the model-facing settings that change the model's behaviour, so an active Session does not keep working from a value another client or process has already changed. It owns the projection, not the settings: each value stays in its owner's source and is read through the owner's public port.

## 1. Scope and non-goals

Owns:

- the criterion for a model-facing setting;
- the first state source `dim/settings/execution` and its body;
- the branch trigger that adopts it on a change and when the operator's input arrives.

Does not own:

- the `execution` Settings document, its schema, CAS or management; that is the App Server's `ExecutionSettings` ([ADR 011](../../adr/011-execution-settings.md));
- the sandbox or network enforcement; that is HostPlatform and the process boundary;
- branch-scoped settings such as the permission preset;
- settings writes by the agent; that is [AgentSettings](AgentSettings.md).

## 2. Which settings are model-facing

A setting is model-facing when the model's behaviour, or its interpretation of its input or output, depends on the effective value. The test is not "the user can change it".

| Setting | Model-facing? | Why |
| --- | --- | --- |
| Execution `sandbox` and `network` | Yes | The model plans commands; the value decides what a new process can do and why a command may fail |
| Reply language | Yes | The model writes to the operator in it; [PromptSources §3](PromptSources.md#3-the-agent-prompt-language-state-source) already designs the block |
| Exec shell | Yes, existing | The environment state already carries it, with the shell guidance |
| Model, effort, fast mode | Existing, not duplicated | They reach the request through the adopted configuration |
| Tools, MCP servers, Skills | Existing, not duplicated | Tool declarations, adopted configuration and `dim/skills/list` already carry them |
| Permission preset | Branch-scoped | Every branch can differ; the current Session-wide source cannot express it (§6) |
| Theme, mouse, notifications, startup form, TUI language | No | Client presentation; the model does not act on it |
| Compaction thresholds, summary reasoning | No for the model | They change context handling, not what the model should do; the summarizer reads them through its own path |
| Hooks, approval rules, session grants | No | They are enforcement; a setting is not a permission and the model does not need the rule text |
| Credentials, tokens, `writeOnly` fields | Never | Secrets |

A general "something changed" digest is not a fact the model can act on; this design does not add one. Each model-facing setting's owner states its own value.

## 3. The first source: `dim/settings/execution`

| Field | Value |
| --- | --- |
| Name | `dim/settings/execution` |
| Delivery | `State` (`<dim-state>`) |
| Order | `240`, after presence (`230`) and before Skills (`300`) |
| Author | EnvironmentSettings, reading the `ExecutionSettingsHost` port |
| Class | Fact |
| Lifecycle | Registered by the Session part, withdrawn with it |

The body states the effective values and their scope of application:

```text
<dim-state source="dim/settings/execution">Execution sandbox: off. New processes run with the host's normal access.</dim-state>
```

```text
<dim-state source="dim/settings/execution">Execution sandbox: on; network: off. New processes only; a running shell keeps its original boundary.</dim-state>
```

```text
<dim-state source="dim/settings/execution">Execution sandbox: on; network: on. New processes only; a running shell keeps its original boundary.</dim-state>
```

- `network` is stated only when the sandbox is on; without the sandbox the switch has no effect and naming it would suggest a guarantee the runtime does not make.
- The "new processes only" sentence is a runtime guarantee (protocol); it changes with the enforcement, not as policy.
- The body carries no revision, timestamp, author or document dump.
- The values are not secrets; no `writeOnly` field is read.

The source is written by the capability, but its values belong to `ExecutionSettings`. The capability reads the public `ExecutionSettingsHost`; it does not read the Settings document or the process environment directly.

## 4. Adoption

The source has one trigger: a periodic check while no turn is open.

- The branch part follows `turn/start` and `turn/end` only to know whether a turn is open. While idle it reads `ExecutionSettingsHost` every 500 ms; when the values differ from the last adopted snapshot it asks Prompt to adopt the source.
- The initial configuration adopts the source at branch open, so the first request of a branch carries the current value. A change during a turn waits for the turn to end, so the block is written while idle before the next input and the operator's message stays the last message.
- Prompt's serial adoption queue coalesces; several writes before the adoption collapse into one read. Prompt refuses an adoption after the branch closes.

## 5. Error and absence

| Situation | Result |
| --- | --- |
| `ExecutionSettingsHost` is absent | The feature is not installed or registers no source; no block |
| The host read fails | Keep the adopted value; log; write no block |
| The value is unchanged | No block |
| A non-model field of the same Settings source changes | No block |
| The setting is genuinely removed | The owner returns no entry; Prompt writes `cleared="true"` for the key. The execution source is built in and has a default, so this does not happen in v1 |
| The branch is closing | No adoption |

A read failure is not a value. The design does not write `unknown` or `cleared` for it.

## 6. Branch-scoped settings

The permission preset is per branch: `permission/changed` is a branch fact and a child branch can differ from its parent. A Session-wide prompt source cannot render it, and the Session part cannot read a branch's state ([agent environment §4](../agent-environment.md#4-scope)).

Version 1 therefore does not publish the permission preset. It also does not publish `main`'s preset as if it were every branch's. A later design may add a branch-scoped source route (a branch part registers a source for its own branch scope) or a different marker; that is a Prompt/PromptSources decision, not a hidden default here.

`dim/language` is designed in PromptSources but has no value carrier yet ([PromptSources §3](PromptSources.md#3-the-agent-prompt-language-state-source)); this issue does not invent one.

## 7. Noise

- Low frequency: settings changes are rare; immediate adoption is acceptable, but a burst is coalesced.
- The comparison is the body, not the Settings revision. A source that contains non-model fields must not rewrite the block when only those change; the owner renders the model-facing subset.
- No timestamps, revisions or "last changed by".
- Per-branch local copies are expected: each open line adopts its own block.
- A setting that the operator changes and changes back produces two blocks (the two values), not a stream; the old block stays until compaction.

## 8. Dependencies

| Service | Purpose |
| --- | --- |
| `P:ExecutionSettingsHost` | Read the effective sandbox and network |
| `S:PromptRegistry` | Register `dim/settings/execution` |
| `B:Dispatcher` | Track whether a turn is open |
| `B:dyn ConfigureControl` | Adopt the source |

The feature is installed only where `ExecutionSettingsHost` is provided. A fixed host (evaluation, a native profile) may install it with fixed values; the product App Server provides the real source.

## 9. Rule check

| Rule | How this design complies |
| --- | --- |
| §1 Parts bounded | Session part registers the source; branch part adopts; the value owner is the App Server's execution settings. |
| §4 rule 1 Command events | The design changes no setting; it reads one. Writes are [AgentSettings](AgentSettings.md). |
| §4 rule 2 Handover | No handover. |
| §4 rule 3 Before callbacks | None. The check task reads the host and asks Prompt to adopt; the Dispatcher handler only tracks the turn. |
| §4 rule 4 Core carries no business meaning | The body is the capability's projection of a public port; no core branch. |
| §4 rule 5 Batch | No new facts. |
| §4 rule 6 Context from consumed events | The block is an adopted input; no second path. |
| §4 rule 7 Public services | `ExecutionSettingsHost` is the owner's public port; the source is a registered prompt source. |
| §4 rule 8 Infrastructure unchanged | No Timeline, Dispatcher, envelope, assembly or facade change. |
| §4 rule 9 Timeline holds decision facts | Settings stay in their source; the state block is an input, not a Timeline fact. |
| §4 rule 10 Adopted inputs | The model reads only the Prompt state input. |
| ADR 002 D1 | The value belongs to the process/Session; the branch part only adopts. |
| ADR 002 D2 | No adapter plugin; the projection is the capability's own model-facing purpose. |
| ADR 002 D3 | The Session part depends on the process port; no core depends on the feature. |
| ADR 002 D4 | No new configuration; the `execution` source stays with its owner. |
| ADR 002 D5 | The port and the body are data; Rust is a binding. |

## 10. Verification

| Scenario | Expected |
| --- | --- |
| Session open, another client turns sandbox on | The next request after the change carries the new execution state; no reload |
| Sandbox back off | A new block with the off value; no stale "on" |
| A non-model field of the same document changes | No block |
| The same value is written twice | No second block (body equality) |
| Session idle when the setting changes | No turn; the next idle check writes the block before the next input |
| Restore | The current value, not the stored block as "now" |
| Host read fails | The old block stays; no `unknown`/`cleared` |
| No host | No block, no error |
| Compaction | The latest execution block is replayed; old ones are gone |
| Permission preset changes | No block from this source; the branch-scoped decision is separate |
| TUI-only setting changes | No block |

## 11. Open decisions

1. The branch-scoped source route for permission preset and other per-branch settings.
2. Whether the source also adopts at each model request (the #986 boundary) instead of only on the idle check.
3. Whether `dim/language` joins this capability or stays in PromptSources once its value carrier exists.
4. The exact set of later model-facing settings after the first `execution` source is measured.
