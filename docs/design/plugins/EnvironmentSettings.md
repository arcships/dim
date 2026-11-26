# EnvironmentSettings

Status: Draft. Parent: [agent environment awareness](../agent-environment.md). Issue: [#1020](https://github.com/arcships/dim/issues/1020). Rules: [design rules](../../architecture/rust-agent/principles.md) §1, §4 and §5, [ADR 002](../../adr/002-plugin-scope.md), [model-facing prompts](../model-prompts.md). Language: Rust.

EnvironmentSettings delivers model-facing settings as one-time snapshots. It is not a read API and not a Prompt source. A snapshot is appended to the branch's input; the model sees a normal `<dim-state>` input. The setting values remain with their setting modules.

This document owns the settings snapshot contract. The settings tool is [AgentSettings](AgentSettings.md). Presence is [EnvironmentClients](EnvironmentClients.md).

## 1. Decisions

- Context injection is an `input/message` event with an Append-mode Raw block and no key. It is not a read and not a Prompt source.
- No `PromptRegistry` registration, no `LoadPrompt`, no version, no keyed state, no compaction replay, no `ConfigureControl`.
- One snapshot at branch open, and one snapshot per observed change.
- The branch-open snapshot is a `BranchLifecycle::register_opening` receipt: the opening waits for the value and for the `input/applied` the input queue writes for it.
- A snapshot is a normal history input. Compaction may remove it; it is not replayed. This is accepted for a one-time hint.
- The snapshot does not expose source, session, revision, timestamps or visibility metadata. It states the effective value.
- Settings values and validation stay with each setting module. This capability only projects them.
- Secret and `writeOnly` fields never enter a snapshot.
- The shared `dim-state` guide entry is registered by `PromptGuide` ([Prompt](Prompt.md#4-the-built-in-entries-and-their-writers)); this part does not register a second entry.

## 2. Which settings

A setting is projected when it is a user preference or choice that is safe and meaningful for the model to know. The test is not "the model can act on it"; it is "the user chose it".

First set:

| Setting | Snapshot |
| --- | --- |
| Execution `sandbox` / `network` | Yes |
| A plugin's business setting | Yes when the setting module provides a body and no secret |

Not included by default:

- credentials, tokens, `writeOnly` fields;
- model, reasoning effort and fast mode, which already travel in adopted configuration;
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

- The body states the configured switches used for new process preparation, not the revision, author or document. It does not certify sandbox backend availability; an unavailable backend is reported by process execution as `isolation_unavailable` ([ADR 010](../../adr/010-sandbox-opt-in.md)).
- `network` is stated only when the sandbox is on.
- The body is stable for an unchanged value; a no-op write appends nothing.
- A non-model field in the same Settings document does not produce a snapshot.

## 4. When a snapshot is appended

### Branch open

The branch part registers its listener and an opening receipt during `apply`
when the branch is `Loading`. The receipt reads the current value, appends the
snapshot, and returns once the branch dispatched it: the input queue has
written the snapshot's `input/applied` by then, so the branch's opening seals
with both committed and dispatched. `session.create` and a restore therefore
return with the snapshot in the branch's published history, and the first
request carries the value. The read is bounded by half the branch's own
opening budget, and by the part's retry interval when that is shorter, so a
source that does not answer cannot outlast the opening: the waiting ends, the
part logs it, and `pending` stays set, leaving the snapshot to the published
branch. The budget is the inactivity interval the branch's own `finish_opening`
is given, so a branch configured with a shorter budget shortens the read with
it.

A child line's own history starts with TimelineTree's identity input, which the
tree writes after the branch opening seals. The receipt therefore appends
nothing for such a line. It decides from the opening kind the line's owner
wrote — the root line's `branch/start`, a child line's `branch/start` naming the
parent it was cut from, or a stored line's `branch/resume` — which the tree
hands the opening receipts when it seals the opening. The receipt makes no
storage read, so the opening cannot be delayed by one and the decision does not
depend on whether the part mounted before the opening event was dispatched. The
input that opens the line stays its first input, and the first loop pass after
`BranchLifecycle::activated()` reads the value and appends the snapshot, so it
follows that input.

A branch whose opening sealed before the part mounted, such as an
`ExecutionSettings` reload onto a live branch, registers nothing either; the
first loop pass after `BranchLifecycle::activated()` appends the snapshot
there. A failed or timed-out opening read, and a failed opening append, keep
`pending`, so that pass retries.

A receipt and the loop after activation check their own generation's
cancellation token right before `Timeline::append`; a child line never reaches
that append, because its opening kind decides first. A cancellation that lands
after the check can leave a second snapshot of the same source, as a reload
does: both events stay in the branch's history, and the later `<dim-state>`
block of the same source replaces the earlier one for the model
([Prompt](Prompt.md#4-the-built-in-entries-and-their-writers)).

A snapshot this part appends after the opening is published before the branch
dispatches it, so until the branch consumes it the branch has an event its
Dispatcher has not taken. A temporary route adoption on that branch is refused
with `provider_route_conflict` for that reason, even though the Session is idle
and the caller is its only client ([route admission](../appserver/provider-management.md#connection-scoped-provider-routes));
the same call is adopted once the branch has dispatched the snapshot and its
`input/applied`, so the caller retries the call or waits for `session.idle`
first. The opening receipt's own append is inside the sealed opening and leaves
no such window: `session.create` and a restore whose settings source answered
return with nothing undispatched.

### Settings change

The Settings service emits:

```text
SettingsChanged {
  source,
  scope,
  revision
}
```

Every open branch listening to that source wakes, reads the current value and appends the snapshot when the body differs. The execution source is user-scoped; its host reads that user-scope value. There is no origin routing and no turn-boundary refresh. An `Append` input does not drive a turn, so an idle Session gets a new history input but no turn.

The snapshot uses the existing `input/message` event; no settings event type is added to the Timeline.

## 5. Errors and absence

| Situation | Result |
| --- | --- |
| The setting module is absent | No snapshot; no error |
| The value read fails | Keep earlier snapshots; log; append nothing |
| The opening read or append fails | Log; keep `pending`; the published branch's first loop pass retries |
| The opening read does not answer within half the branch's opening budget, or within the part's retry interval when that is shorter | Log; keep `pending`; the published branch's first loop pass reads and appends it |
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
| `BranchLifecycle` | Register the opening receipt, which the opening hands the line's opening kind; wait for publication before appending afterwards; stop when the branch closes; its generation token is checked before an append |
| `Dispatcher` | The opening receipt returns once the branch dispatched the snapshot |
| `Timeline` | Append the snapshot |
| Process bus (`SettingsChanged`) | Wake on a committed write |

The execution source's process `executionSettingsHost` is a data face with asynchronous `read({})`; `timeline`, `branchLifecycle` and `dispatcher` are data faces. This Rust projection is not a leaf-ready foreign part.

The App Server profile requires `SettingsSnapshotBranch` (`AgentBuilder::require_part`) where it installs `ExecutionSettingsPlugin`, so a branch opening never seals before the part registered its opening receipt.

No `PromptRegistry`, no `ConfigureControl`, no prompt source, no version.

### Requirements raised to mechanism owners

None to the runtime or rutis.

## 7. Rule check

| Rule | How this design complies |
| --- | --- |
| §1 Parts bounded; Capability | The setting value stays with its module; `SettingsSnapshotBranch` is a Capability because it writes snapshot input facts through its branch Timeline. No new process part is added. |
| §4 rule 1 Command events | The design changes no setting. The settings tool is [AgentSettings](AgentSettings.md). |
| §4 rule 2 Handover | No handover. |
| §4 rule 3 Before callbacks | The opening receipt appends before the branch is Active; later appends run in the part's own task and the event handler only marks work and returns. |
| §4 rule 4 Core carries no business meaning | The body is the setting module's projection; no core branch. |
| §4 rule 5 Batch | A snapshot append is one branch input. |
| §4 rule 6 Context from consumed events | The snapshot is an input; no second context path. |
| §4 rule 7 Public services | The setting module's read port is the only source. |
| §4 rule 8 Infrastructure unchanged | No Timeline, Dispatcher, envelope, assembly or facade contract change; the part reads the existing `branchLifecycle` and `dispatcher` faces. |
| §4 rule 9 Timeline holds decision facts | Settings stay in their source; only the delivered snapshot is an input fact. |
| ADR 002 D1 | The value belongs to the setting module; the branch part only appends. |
| ADR 002 D2 | No adapter plugin; the capability projects its own fact. |
| ADR 002 D3 | The branch part depends on the setting module's public port; no core depends on the capability. |
| ADR 002 D4 | No new configuration; settings stay with their module. |
| ADR 002 D5 | The body is data; Rust is a binding. |

## 8. Verification

| Scenario | Expected |
| --- | --- |
| Branch open | The snapshot and its `input/applied` are committed and dispatched before the branch is Active, so `session.create` and a restore return with both |
| A child line opens | The identity input is the line's first input; the snapshot follows it once the line is published |
| A part mounts after the line's `branch/start` was dispatched | It decides from the opening kind the tree hands the receipt: the receipt writes nothing ahead of the identity input, and the snapshot follows once the line activates |
| The Timeline store stalls during the opening | The receipt makes no read of it: the branch opens within its budget, and a child line's identity input stays first |
| The settings source does not answer during the opening | Log; the branch opens; the first loop pass after activation reads and appends one snapshot |
| The branch's opening budget is shorter than the retry interval | The read is bounded by half that budget; the branch opens and the first loop pass appends one snapshot once the source answers |
| A generation is cancelled before its receipt's append | The receipt checks the token right before it appends and appends nothing |
| A generation is cancelled after its receipt's last check | It may append one more snapshot, as a reload does: both events stay in history, and the later `<dim-state>` block of the same source replaces the earlier one for the model |
| A temporary route adopted with no `session.idle` after create or restore | The opening read answered: adopted; the opening left nothing undispatched |
| A temporary route adopted while a missed opening read's snapshot is still being dispatched | Refused with `provider_route_conflict`; the same call is adopted once the branch dispatches the snapshot and its `input/applied`; the caller retries or waits for `session.idle` |
| Opening sealed before the part mounted | The snapshot is appended after activation |
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
