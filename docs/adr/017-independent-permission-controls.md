# 017: Independent file access and tool approval

Decision status: Accepted.

## Context

A permission preset combines file scope with risk-based tool admission. Naming that combination
Ask or Full access obscures whether a choice changes files, tool questions or process isolation.
Changing the meaning of an existing preset would also reinterpret persisted Session configuration
and `permission/changed` history.

## Decision

### D1. Add optional, independent policy overrides

Extend the public `PermissionPolicy` carried by Session configuration and `permission/changed`
with optional `fileAccess` (`read-only`, `workspace-read-write`, `unrestricted`) and `toolApproval`
(`ask`, `bypass`). Keep `preset`, `allowedTools` and `disallowedTools`.

An absent override preserves that dimension's exact legacy behavior. An explicit `fileAccess`
changes only file scope; an explicit `toolApproval` changes only risk-based tool admission.
In particular, deriving an Ask label from an old preset does not turn that old policy into
explicit Ask: legacy policies can allow some risks automatically and deny others. A client must
show that distinction and must not silently widen an old policy.

Explicit Ask asks for every tool call unless an explicit tool allow or matching command allow
answers that tool question. Bypass skips tool questions only. Explicit denies always win.
The file gate uses the existing fixed facts and grants: tool allows and Bypass cannot skip it,
Read only denies writes, and a read grant cannot authorize a write. A call needing both file-scope
and tool consent uses one call-specific approval, not two sequential prompts. Neither override
changes process isolation.

The facts query receives effective file access instead of a mixed preset; its fixed facts,
owners and error contract are unchanged. The language-neutral service shape is owned by the
[Permission page](../architecture/rust-agent/plugins/Permission.md#file-access-facts).

### D2. Keep old records and legacy entry points intact

This is an additive policy extension, not a replacement persistence format. Omitted fields retain
their previous serialized form and interpretation. Do not rewrite existing Timeline events,
Session/database records or frozen compatibility fixtures. New explicit values are carried by
the existing versioned policy update; no new event kind or domain is required. Legacy wire
presets and `--permission` remain supported. Older clients or servers that reject the additional
fields are not assumed to support explicit controls; clients report the failure rather than
falling back to a broader preset.

### D3. Separate the visible controls

The TUI exposes File access, Tool approval and Process isolation. `/permissions` is the canonical
menu command; `/approvals` and `/approval` are compatibility aliases. Ask and Bypass are the tool
choices; Auto is reserved for a future actual automatic decision system, not an alias for Bypass.
The old Full access label is not a new TUI choice.

Process isolation reuses the existing execution settings and backend. Its single visible control
replaces Settings > Execution and any separate Sandbox entry. CLI interaction and remembered
choices belong to the [CLI design](../design/cli-commands.md#permission-controls); process
boundaries remain the [HostPlatform contract](../design/host-platform-isolation.md).

## Consequences

- Permission owns the additive public policy semantics; clients edit the relevant field while
  retaining the other fields. The [Permission page](../architecture/rust-agent/plugins/Permission.md)
  owns the decision contract and compatibility details.
- Independent TUI choices can be remembered for future CLI Sessions alongside the old config;
  this does not create a daemon-wide default for other clients.
- File access is tool admission, not filesystem isolation for arbitrary shell commands. Sandbox
  settings retain their process owner, persistence source and lifetime.

## Non-goals

An automatic approval answerer, new facts or grants, a new sandbox backend, changing configured
command authority under ADR 003, and migrating historical policy records.

## Supersession

Extends ADRs 009 and 014 with independent explicit policy overrides and an additive public
policy-format decision. Supersedes ADR 011's visible Execution/two-switch presentation, not its
ExecutionSettings ownership, stored booleans or host port. The prior ADR files remain unchanged.

## Rule check

| Rule | Application |
| --- | --- |
| §1, ADR 002 D1 | Permission and Approval remain branch Capabilities; facts remain Session-owned, isolation process-owned. No new part. |
| §4 rule 1 | Policy changes use the existing Permission update and committed `permission/changed`. |
| §4 rule 2 | Execution handover is unchanged; consent does not reclaim an executing call. |
| §4 rule 3 | No `before` callback or commit-time I/O. |
| §4 rule 4 | Policy semantics stay in Permission, not Timeline or Dispatcher. |
| §4 rule 5 | Existing policy updates and approval facts keep their batch and dispatch contract. |
| §4 rule 6 | Only existing adopted inputs and tool results enter context. |
| §4 rule 7 | Clients use public policy and Settings operations; Permission reads existing facts and command rules. |
| §4 rule 8 | No infrastructure change or runtime requirement. |
| §4 rule 9 | Only policy needed for later decisions extends the existing fact; remembered UI and execution settings stay out of Timeline. |
| ADR 002 D2 | No wiring or adapter part. |
| ADR 002 D3 | No new dependency or scope crossing. |
| ADR 002 D4 | Business policy and settings do not change structural plugin configuration. |
| ADR 002 D5 | Optional fields and values are language-neutral public data. |

## Requirements raised to mechanism owners

None. Existing policy updates, facts, approval requests and execution settings suffice.
