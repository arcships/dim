# PromptSourcesBranch

Scope: branch. Issue: #2027.

PromptSourcesBranch keeps a line's checked prompt sources level with what Prompt adopted: AGENTS.md instructions, the runtime environment and date, and the reply language. The sources themselves and their texts are PromptSources' Session part, designed in [PromptSources](PromptSources.md) and owned by #2026; this document is the branch part's target contract.

## Use and scope: responsibility split

PromptSourcesBranch owns:

- **The check.** One tracked task per line: load the checked sources through PromptHost at the configured interval, compare each with Prompt's effective entry for its target, and collect the differences.
- **The adoption request.** Hand the changed values to Prompt's `configure` as data; the part decides nothing about entries, the System or the model.
- **The generation.** The task starts after the line is published, ends when closing starts, and a reload starts a fresh comparison from the current adopted entries.

Callers keep as business:

- The source texts and which sources are checked: PromptSources.
- The adoption, the System and the configuration facts: Prompt.
- The reply language's live value: the Session's language owner; the language source reads it through PromptSources.
- The polling interval: the assembly's factory configuration.

| Requirement (caller) | Can callers compose it from the operations below without breaking PromptSourcesBranch's invariants? | Handling |
| --- | --- | --- |
| A line keeps its guide and environment entries current without a request rebuilding them | Not a caller requirement: the part is the reader itself | No operation; the part is the caller of Prompt and PromptSources |
| A client forces a refresh | Yes: Prompt's `configure` with the loaded values | Business composition by the client's own entry |

The part has no public operation: it holds a task and a generation, not state another owner asks for. Removing the check from a profile removes the part; no caller substitutes for it.

## Caller example

The part's own loop, after the change; only PromptSources' load and Prompt's operations appear:

```rust
let AdoptedEntries { entries } = prompt
    .adopted_entries(AdoptedEntriesRequest { targets: checked.clone() })?;
let mut changed = Vec::new();
for target in checked {
    let LoadReply { entry } = prompt_sources
        .load(target.source.clone(), target.scope, cancelled.clone())
        .await?;
    let adopted = entries
        .iter()
        .find(|(source, _)| source == &target.source)
        .map(|(_, entry)| &entry.version);
    if entry.as_ref().map(|entry| &entry.version) != adopted {
        changed.extend(entry);
    }
}
if !changed.is_empty() {
    prompt.configure(ConfigureRequest::Loaded { sources: changed }, cancelled).await?;
}
```

## Own contract

### Dependencies

| Owner | Operation | Use |
| --- | --- | --- |
| PromptSources (#2026, Session) | `load` | One checked source's current entry for a target. |
| Prompt | `adoptedEntries`, `configure` | The effective entries and the adoption of the differences. |
| BranchScope | `branchLifecycle` | Activation before the first check and the closing gate. |

### Operations

None. The part is a caller of Prompt and PromptHost; it exposes no service and registers no tool or method.

### Errors

The part has no caller-facing errors. A source load that fails is logged and retried at the next interval; an adoption that is `superseded` is not retried because a newer adoption already carries the values; a `closed` answer ends the task.

### Flows

| Flow | Normal path | The line closes | The part reloads |
| --- | --- | --- | --- |
| the check | Wait for activation; check immediately, then every configured interval; compare each checked source with Prompt's effective entry; adopt the differences in one `configure` | The task ends at closing; no adoption is written | A new generation checks immediately and compares against the current adopted entries; the old task is joined |
| a failed source load | Log the source and the error; keep the adopted entry; retry at the next interval | The task ends | The new generation retries |
| an adoption failure | Log it; the next interval sees the same difference and retries; a `superseded` answer is not retried | The task ends | The new generation compares again |

The task runs outside the owner's locks; the part registers it in `apply` and cancels and joins it at unload, so no check outlives its generation.

## Exceptions with reasons

- The part polls instead of subscribing: prompt sources are files, environment values and a date, with no publication of their own; the interval is the only way to observe a change, and the current design keeps it.
- The comparison reads Prompt's effective entries rather than a snapshot of the whole projection: the part needs only the targets it checks, and the whole projection would duplicate Prompt's state.

## Supersedes

- [PromptSources](PromptSources.md): the PromptSourcesBranch section and its `PromptSourcesBranch::language` cell forwarding.
- `docs/design/service-faces.md`: the `promptSourcesBranch` catalog row and the section 4.8 statement that the branch re-exposes the Session language cell.

## Verification

- A change in AGENTS.md or the environment is adopted at the next check and appears in the line's System; an unreadable project file keeps the adopted version.
- A day change appends the date entry once; a language change is adopted for every open line through Prompt.
- The task starts only after activation, ends when closing starts, and a reload compares against the current adopted entries.
- A failed load writes no adoption and is retried; a superseded adoption is not repeated.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One branch part; it writes no Timeline fact of its own and folds no state, so it is not a Capability. |
| §2 scopes | Branch: the check follows one line's adopted entries. |
| §3 orthogonality | No other owner's state is read; the sources and the entries arrive through their owners' operations. |
| §4 rule 1 | No command event: the adoption is Prompt's operation. |
| §4 rule 2 | No handover. |
| §4 rule 3 | The tracked task does the loads; no handler waits. |
| §4 rule 4 | No business meaning: the texts are the sources'. |
| §4 rule 5 | The differences go in one `configure`. |
| §4 rule 6 | The entries reach the model only through Prompt's adopted System. |
| §4 rule 7 | The part uses PromptHost and Prompt; it reads no Timeline state. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | The part writes no fact. |
| §7 values, ids and objects | The loaded values are data; no object crosses. |
| §7 one interface | No public name; the `promptSourcesBranch` forwarding service is removed. |
| §7 cross-language | The part exposes nothing; its dependencies are data faces. |
| ADR 002 D1 | Branch: the check belongs to one line. |
| ADR 002 D2 | No wiring plugin: the part is the feature's branch part. |

## Requirements raised to mechanism owners

- #2026 (PromptSources): the checked sources, their texts and the reply language's live value; the part loads them through `promptSources.load` and adds no second source path.
- #2026 (Session language): the live language value is read by the language source through PromptHost; the branch part no longer re-exposes a Session cell.

## Binding appendix

No service block: the part provides no public operation. The part is registered by the PromptSources feature as its branch part and is removed when the feature is not installed.
