# EnvironmentSettings

Status: Draft. Parent: [agent environment awareness](../agent-environment.md). Issue: [#1020](https://github.com/arcships/dim/issues/1020). Rules: [design rules](../../architecture/rust-agent/principles.md) §1, §4 and §5, [ADR 002](../../adr/002-plugin-scope.md), [model-facing prompts](../model-prompts.md). Language: Rust.

EnvironmentSettings delivers model-facing settings as one-time snapshots. It is not a read API and not a Prompt source. A snapshot is appended to the branch's input; the model sees a normal `<dim-state>` input. The setting values remain with their setting modules.

This document owns the settings snapshot contract. The settings tool is [AgentSettings](AgentSettings.md). Presence is [EnvironmentClients](EnvironmentClients.md).

## 1. Decisions

- Context injection is `input/append` of a snapshot. It is not a read and not a Prompt source.
- No `PromptRegistry` registration, no `LoadPrompt`, no version, no keyed state, no compaction replay, no `ConfigureControl`.
- One snapshot at branch open, and one snapshot per observed change.
- A snapshot is a normal history input. Compaction may remove it; it is not replayed. This is accepted for a one-time hint.
- The snapshot does not expose source, session, revision, timestamps or visibility metadata. It states the effective value.
- Settings values and validation stay with each setting module. This capability only projects them.
- Secret and `writeOnly` fields never enter a snapshot.

## 2. Which settings

A setting is projected when it is a user preference or choice that is safe and meaningful for the model to know. The test is not "the model can act on it"; it is "the user chose it".

First set:

| Setting | Snapshot |
| --- | --- |
| Execution `sandbox` / `network` | Yes |
| Per-session model, reasoning effort, fast mode | Yes when the setting module provides a body |
| A plugin's business setting | Yes when the setting module provides a body and no secret |

Not included by default:

- credentials, tokens, `writeOnly` fields;
- permission presets, approval rules, session grants;
- hooks, MCP server configuration, plugin enablement;
- model log and other diagnostics;
- TUI-only preferences such as theme and interface language, unless the client preference path is separately designed.

The setting module owns the body. It may state the effective value and what it applies to; it must not dump the Settings document.

## 3. Snapshot body

The body is a `<dim-state>` input:

```text
<dim-state source="dim/settings/execution">Execution sandbox: off. New processes run with the host's normal access.</dim-state>
```

```text
<dim-state source="dim/settings/execution">Execution sandbox: on; network: off. New processes only; a running shell keeps its original boundary.</dim-state>
```

```text
<dim-state source="dim/settings/execution">Execution sandbox: on; network: on. New processes only; a running shell keeps its original boundary.</dim-state>
```

Rules:

- The body states the effective value, not the revision, author or document.
- `network` is stated only when the sandbox is on.
- The body is stable for an unchanged value; a no-op write appends nothing.
- A non-model field in the same Settings document does not produce a snapshot.

## 4. When a snapshot is appended

### Branch open

The branch part registers its listener during `apply` and appends the current snapshot once after `BranchLifecycle::activated()`. The first request may race that append; a later request carries the value.

### Settings change

The Settings service emits:

```text
SettingsChanged {
  source,
  revision
}
```

Every open branch wakes, reads the current value and appends the snapshot when the body differs. There is no origin routing and no turn-boundary refresh. An `Append` input does not drive a turn, so an idle Session gets a new history input but no turn.

No Timeline event is written.

## 5. Errors and absence

| Situation | Result |
| --- | --- |
| The setting module is absent | No snapshot; no error |
| The value read fails | Keep earlier snapshots; log; append nothing |
| The value is unchanged | Append nothing |
| A non-model field changes | Append nothing |
| The branch is closing | Append nothing |
| The setting is genuinely removed | The module returns no body; no new snapshot. A previous snapshot stays as the last known value until compaction |

A read failure is not a value. The design does not append `unknown` or a fabricated default.

## 6. Dependencies

| Dependency | Purpose |
| --- | --- |
| The setting module's read port | Read the effective values for the body |
| The branch's input path | Append the snapshot |
| Process bus (`SettingsChanged`) | Wake on a committed write |

No `PromptRegistry`, no `ConfigureControl`, no prompt source, no version.

### Requirements raised to mechanism owners

None to the runtime or rutis.

## 7. Rule check

| Rule | How this design complies |
| --- | --- |
| §1 Parts bounded; Capability | The setting value stays with its module; `SettingsSnapshotBranch` is the Capability, and there is no process part. |
| §4 rule 1 Command events | The design changes no setting. The settings tool is [AgentSettings](AgentSettings.md). |
| §4 rule 2 Handover | No handover. |
| §4 rule 3 Before callbacks | The branch part appends in its own task; the event handler only marks work and returns. |
| §4 rule 4 Core carries no business meaning | The body is the setting module's projection; no core branch. |
| §4 rule 5 Batch | A snapshot append is one branch input. |
| §4 rule 6 Context from consumed events | The snapshot is an input; no second context path. |
| §4 rule 7 Public services | The setting module's read port is the only source. |
| §4 rule 8 Infrastructure unchanged | No Timeline, Dispatcher, envelope, assembly or facade change. |
| §4 rule 9 Timeline holds decision facts | Settings stay in their source; the snapshot is an input, not a Timeline fact. |
| §4 rule 10 Adopted inputs | The model reads the appended input directly. |
| ADR 002 D1 | The value belongs to the setting module; the branch part only appends. |
| ADR 002 D2 | No adapter plugin; the capability projects its own fact. |
| ADR 002 D3 | The branch part depends on the setting module's public port; no core depends on the capability. |
| ADR 002 D4 | No new configuration; settings stay with their module. |
| ADR 002 D5 | The body is data; Rust is a binding. |

## 8. Verification

| Scenario | Expected |
| --- | --- |
| Branch open | A current snapshot; the first request may race it |
| Another client changes sandbox | Every open branch appends the new snapshot; an idle branch gets no turn |
| Same value written twice | One snapshot, no duplicate |
| Non-model field changes | No snapshot |
| Idle Session when a setting changes | A history snapshot, no turn |
| Host read fails | Earlier snapshot stays; no fabricated value |
| Compaction | A snapshot may be removed; no replay |
| Secret field changes | No snapshot |
| Branch closing | No append |

## 9. Open decisions

1. The exact set of settings after execution is measured.
2. Whether TUI-only preferences get a client preference path and a snapshot.
3. Whether a snapshot should ever be replayed after compaction; the current answer is no.
