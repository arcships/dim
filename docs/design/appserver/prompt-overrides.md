# Prompt overrides

Status: Draft ([#1314](https://github.com/arcships/dim/issues/1314)). Requirements:
[model-facing prompts §9.2](../model-prompts.md#92-every-capability-that-writes-model-facing-text),
[PromptSources §2](../plugins/PromptSources.md#2-policy-at-the-factory-configuration),
[ContextCompaction §13.5](../plugins/ContextCompaction.md). Rules: [design
rules](../../architecture/rust-agent/principles.md) §1 and §4, [ADR 002](../../adr/002-plugin-scope.md)
D1 and D4, [ADR 007](../../adr/007-plugin-configuration.md) D5, [settings
service](settings-service.md). Scope: process, inside the existing Settings plugin and one App
Server row; the texts stay in their Session parts.

`base` and `summary` are policy: the runtime behaves the same whatever they say. `skills` is
mixed — its override also replaces the protocol sentences that name the marker and the call
shape, while the `skill` tool and the `dim/skills/list` state source do not change. Each text
already has a factory configuration member. This design adds one user setting that fills a
member its assembly left unset; it does not move the text, add a context path or make the
capability read a second source.

## 1. Summary

The App Server registers one Settings source, `prompts` (user scope only), with three independent
optional strings: `base`, `summary` and `skills`. It provides three optional host ports, one per
owning capability. Each Session part reads its own port while it applies and keeps the text it
adopted. Precedence is:

```text
factory configuration (a candidate assembly) > user setting > built-in text
```

A missing field, a missing port or a read failure keeps the built-in text. A change applies to a
new Session. A restored Session keeps the `base` and `skills` System entries it persisted; the
summary strategy reads its setting when the restored Session's part applies, so a changed
summary prompt applies to that Session's next compaction. A loaded Session keeps the text it
adopted.

```text
                       App Server process row: PromptSettings
                         registers Settings source "prompts"
                         provides three ports from the same document
                                        |
             +--------------------------+--------------------------+
             |                          |                          |
  BasePromptSettingsHost   SkillsInstructionsSettingsHost   SummaryPromptSettingsHost
             |                          |                          |
     PromptSources part            Skills part          ProductSummaryStrategy part
        (dim/base)             (dim/skills/instructions)      (summary prompt)
             \                          |                          /
              read once while the Session part applies
```

## 2. The document

`prompts.json`, source `prompts`, user scope only. Each field is independent; an absent field
keeps that text's built-in value.

```json
{
  "base": "…",
  "summary": "…",
  "skills": "…"
}
```

| Field | Replaces | Required shape |
| --- | --- | --- |
| `base` | `dim/base`'s identity and rules | non-empty string with a non-whitespace character |
| `summary` | the compaction summarizer's system prompt | the same |
| `skills` | the how-to-use-Skills instructions | the same |

The schema (`type: object`, `additionalProperties: false`) refuses an unknown field, an empty
string and a whitespace-only string. A whole-text replacement is the only operation; an empty
draft is not a write, and removing an override is an explicit reset.

**No project file.** A project file would let a cloned repository rewrite the agent's policy, so
the source declares no project scope. If one is ever added, every field is user-only and the
project source starts disabled ([ADR 003](../../adr/003-approval-scope.md)).

The product home uses native storage, so the document lives in SQLite and the settings methods
are its normal surface; in the file-backed profile it is `prompts.json` under the home.

## 3. Host ports and precedence

Each owner declares its own port, so a capability depends only on its own text:

| Owner | Port | Reads |
| --- | --- | --- |
| PromptSources | `BasePromptSettingsHost` | `base` |
| Skills | `SkillsInstructionsSettingsHost` | `skills` |
| ContextCompaction | `SummaryPromptSettingsHost` | `summary` |

- The port is optional (`Option<Key<…>>`): an assembly that does not register the name, such as
  the capability tests or a host without Settings, injects nothing and keeps its built-in text.
- The factory configuration outranks the port. The product profile sets none, so the user
  setting is in force; an evaluation passing a candidate keeps the candidate, whatever the
  user's home holds.
- A read failure is logged with the source name and never carries the document's content; the
  Session uses the built-in text rather than failing to open.
- Reading happens in the Session part's `apply`, so the value is fixed for that Session. Nothing
  rewrites the adopted System prompt of a running conversation.

### 3.1 Data operations

The same three read ports are available to plugins of any language:

| Service | Async operation | Request | Reply |
| --- | --- | --- | --- |
| `basePromptSettingsHost` | `base` | `{}` | string or `null` |
| `skillsInstructionsSettingsHost` | `instructions` | `{}` | string or `null` |
| `summaryPromptSettingsHost` | `systemPrompt` | `{}` | string or `null` |

Each reply is that port's existing override text; `null` means no user override.
The ports read through the same PromptSettings owner and source. They add no cache,
extra source read, configuration field or change to the precedence above.

Errors are `{code, message}`. The product owner reports `failed` for a settings read
error, including a settings read ended by cancellation, retaining its existing error
text. Malformed wire requests are `invalid_request`. Dropping a wire call cancels
its read token and abandons its reply; this does not introduce a new product error
code. Native callers retain their existing cancellation token and error behavior;
a failed port read does not change the source.

## 4. Client entry

The settings methods are the primitive: `settings.read` and `settings.update` with
`source: "prompts"`, `scope: "user"`. `settings.sources` publishes the schema, and both methods
are management methods, so only a local connection can edit the texts.

The native client presents each field as plain text rather than JSON:

- `/settings` lists the three texts under `Prompts`, each showing `built-in`, `custom` or
  `Not available`. Enter opens the current text in `$VISUAL`/`$EDITOR`, and `r` on a custom row
  resets it. A saved draft replaces one field and preserves the others; a conflict or a refused
  write keeps the draft. The command menu keeps its curated order because no slash command is
  added.

  ```text
  +------------------------------------------+
  | Settings                                 |
  |   Prompts                                |
  | > Base prompt                built-in >  |
  |   Compaction prompt          custom >    |
  |   Skills instructions        built-in >  |
  | up/down move · Enter edit · r reset      |
  +------------------------------------------+
  ```

- `dimcode prompt [show|edit|set|reset] NAME` is the headless and scripted surface: it needs no
  Session, `edit` reuses the management draft and editor, `set` reads a file or stdin, and every
  write uses the revision it read. Empty input is refused; `reset` removes the field.

A saved change says it applies to new Sessions. Both surfaces warn before a `skills` write that
it replaces the marker and call shape while the tool and the list stay. A daemon without the
source answers the named unsupported-daemon error and the client shows no value.

## 5. Texts that stay closed

This setting opens policy texts that have a safe default and no parsed output. It does not open:

- facts (`dim/environment/*`, `dim/language`) or protocol texts (the input-guide frame, guide
  entries, tool declarations and marker spellings);
- algorithm- or output-coupled texts: the summary length sentence (a `{target}` template), the
  session-title prompt (its reply is parsed), the prompt-suggestion instruction (its reply is
  validated) and the branch tool description (it carries protocol sentences that must stay
  byte-identical);
- the shell and file-encoding guidance: policy, but shaped per shell edition and Windows branch,
  which one string field cannot carry;
- feature-internal policy sentences (Goal's objective and continuation, Collaboration's two
  descriptions): policy, but only meaningful with their feature's protocol and evaluated with
  it;
- text the operator already owns: `AGENTS.md`, the profile or `dim/session` prompt, workflow
  stages, scheduled job prompts, hooks and Skill bodies.

`skills` is a mixed text: an override replaces the whole block, including the protocol sentences
that name the marker and the call shape. Both client paths warn before writing it; the `skill`
tool and the `dim/skills/list` state source do not change, and the flow test asserts both. `base`
is replaced whole as well, so the built-in safety and working rules disappear with it; that is
the operator's explicit choice.

A text is added to this document only when its owner exposes it at factory configuration and a
flow test can assert that the override leaves the marker, declaration or output contract
unchanged. A pure policy text qualifies directly; a mixed text (`skills`) qualifies only with a
client warning and a test that its tool declaration and state source still work.

## 6. Rule check

Rules: [design rules](../../architecture/rust-agent/principles.md) §1 and §4, [ADR
002](../../adr/002-plugin-scope.md), [ADR 007](../../adr/007-plugin-configuration.md) D5, [ADR
008](../../adr/008-documentation-ownership.md).

| Rule | How this design complies |
| --- | --- |
| §1 Plugins and capabilities bounded | Each text stays in its owner; the App Server owns only the Settings document and the ports |
| §4 rule 4 No business meaning in core | No core type gains a prompt text; the ports are capability-level contracts |
| §4 rule 6 Context from consumed events | The text reaches the model through the existing adopted prompt sources; no new context path |
| §4 rule 7 Public events and services | No new event, client method or Timeline fact; the existing Settings methods and three optional process services are used |
| §4 rule 8 Infrastructure unchanged | No change to Timeline, Dispatcher, rutis or the assembly mechanism |
| ADR 002 D1 Scope | The document and ports live at the process scope where the shared configuration is; the texts live in their Session parts |
| ADR 002 D4 Configuration | Business configuration stays with its owner; the document is the App Server's source, as `execution` and `prompt-suggestion` are |
| ADR 007 D5 Owner registration | A dedicated App Server row registers the source; Settings and the assembly declare none |
| ADR 008 D2 No delivery status | This document states the design; the issue tracks delivery |
| ADR 008 D3 Own scope only | The owner documents state their own port; this page states the product setting and the client entry |

## 7. Binding appendix

```rust
pub trait BasePromptSettings: Send + Sync + 'static {
    fn base(&self, cancel: CancellationToken) -> HostFuture<'_, Option<Arc<str>>>;
}
pub struct BasePromptSettingsHost(pub Arc<dyn BasePromptSettings>);

pub trait SkillsInstructionsSettings: Send + Sync + 'static {
    fn instructions(&self, cancel: CancellationToken) -> HostFuture<'_, Option<Arc<str>>>;
}
pub struct SkillsInstructionsSettingsHost(pub Arc<dyn SkillsInstructionsSettings>);

pub trait SummaryPromptSettings: Send + Sync + 'static {
    fn system_prompt(&self, cancel: CancellationToken) -> HostFuture<'_, Option<Arc<str>>>;
}
pub struct SummaryPromptSettingsHost(pub Arc<dyn SummaryPromptSettings>);
```

The App Server row registers the `prompts` declaration and provides the three hosts from one
`PromptDocument`; each host reads only its field. Each binding is provided at its Rust type key and
`host_key` for its service name. Its dispatcher calls the same owner method; native
callers invoke that method directly without JSON conversion.
