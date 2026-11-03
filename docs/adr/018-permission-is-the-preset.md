# 018: Permission is the preset; file scope and isolation are sandbox settings

Decision status: Accepted.

## Context

[ADR 017](017-independent-permission-controls.md) D3 put File access, Tool approval and Process
isolation on one `/permissions` panel. Its reason was legibility: a preset name such as Ask or
Full access did not say whether a choice changed files or tool questions.

The panel now asks the user to make three choices, and two of them do not decide permission:

- A preset already decides both questions. `read-only` cannot write, `workspace-write` writes
  inside the workspace and asks outside it, `full-access` does not ask. An independent File
  access control can therefore only produce combinations nobody asks for — paths unrestricted
  while every call still asks, or paths read-only while tool questions are skipped.
- What is genuinely independent of "may this run, and must it ask" is the **path boundary**:
  workspace, system roots, a Session's own files. That is an execution boundary, owned by the
  facts and the sandbox ([014](014-file-access-facts.md), [010](010-sandbox-opt-in.md)).
- Process isolation is an execution backend ([011](011-execution-settings.md), owned by
  ExecutionSettings). ADR 017 brought its control onto the permission panel to replace the
  Settings > Execution entry, which put two owners behind one menu.

## Decision

### D1. One permission control: the preset

`/permissions` offers the three presets and nothing else. It is the decision of what may run and
what must be asked before it runs; the panel states the consequence of the chosen preset rather
than splitting it into dimensions the user cannot act on independently.

### D2. File scope and process isolation move under the sandbox settings

The process isolation backend appears under Settings, beside the execution settings that own it.
`/permissions` links there instead of hosting the control. The path boundary needs no control of
its own: the preset fixes it, and the one exception a user can grant is a Session grant
([ADR 012](012-session-grants.md)), which Permission already consults.

### D3. The two overrides are removed, and this type stops rejecting unknown keys

`PermissionPolicy` loses `fileAccess` and `toolApproval`. The preset is the policy of record: an
override refined it for one client's panel and was never an authority of its own. Admission
follows the preset, and a record that does not name one takes the default; a Session that ran
under a stricter override runs by its preset from here on.

The type also loses `deny_unknown_fields`. A record written before this decision carries the two
keys, and strict parsing would refuse it; with the attribute gone the keys are unknown and are
ignored, like any other key the type does not name. The cost is deliberate and stated: a
misspelling in this type no longer fails to parse, so a typo in a policy reads as the default.

This is a breaking format decision, not a silent reinterpretation: client options, tests and
ADR 017 in the current alpha still write the keys, and this decision stops reading their meaning.

The documents that embed a policy follow the same rule: a Scheduler job's stored policy and a
Workflow run's recorded cap load with the removed keys ignored, are not rewritten on read, and are
written back without them. Only that policy object stops rejecting a key it does not name; the job
and run documents around it stay strict, so a typo anywhere else is still an error.

### D4. Cap and containment compare presets and tool lists

Scheduler's `cap`, `within` and `effective` and Workflow's launch cap keep their authority
comparison, expressed in the presets they still have: the lower preset wins, allowed tools are the
ones both sides allow, disallowed tools are the union. The implementation drops the dimension
comparison — `fileRank`, the derived approval, the schema properties, and the `schedule_invalid`
case for explicit Ask under a legacy read-only bound. `schedule_invalid` keeps its other
validation of a job.

## Consequences

- ADR 017 D3 is superseded: there is one visible permission control, and the two others leave
  the panel.
- `/permissions`, the CLI design and the Permission page describe the preset and link to the
  sandbox for the rest; the Permission page stops documenting an independent file-access
  override as a user choice.
- Clients stop rendering and writing the two overrides; a value they send under those names is
  ignored like any unknown field.
- A record without an override keeps its exact legacy admission. A Session that ran under an
  override admitting less than its preset now admits by its preset, which follows from the preset
  being the policy of record rather than a defect.
- Policy parsing no longer rejects unknown keys — in this type and in the policy object embedded by
  the plugin documents — which removes typo detection there. Other types keep
  `deny_unknown_fields`, and the documents around an embedded policy stay strict.

## Non-goals

- Not changing what a failed admission does, and not changing Session grants or approvals.
- Not changing the sandbox's own contract, its opt-in default or its file facts (010, 014).
- Not changing which calls ask: the preset decides that, as before.

## Supersession

Supersedes [ADR 017](017-independent-permission-controls.md) D3, which put the three controls on one
panel, and D1's two overrides, which this decision removes. Supersedes ADR 017 D2 for the policy
type only: a record is read with the removed keys ignored and is not rewritten, so the promise not
to rewrite stored events and records still holds.

The [Scheduler](../design/plugins/Scheduler.md) and [Workflow](../design/plugins/Workflow.md) pages
lose the dimension comparison and the stored optional fields with D1 and D4; their full-policy
comparison is now presets and tool lists.
