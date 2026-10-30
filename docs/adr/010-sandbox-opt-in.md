# 010: The sandbox is opt-in

Decision status: Proposed.

Extends [ADR 009](009-permission-decides.md): the permission decision is final and HostPlatform
enforces it. This decision replaces the default-on sandbox policy of the
[F5 isolation design](../design/host-platform-isolation.md), its policy file and management
methods, and the `sandbox` argument. ADR 003 is unchanged.

## Context

The current defaults — sandbox on, network off, readable workspace and system directories — are
wrong for a local coding agent. A normal task needs the user's toolchains in the home directory,
the package managers' network and the files the user points at. A release without a policy file
gets those restrictive defaults and the agent cannot do ordinary work. The policy file was the
only escape, and users are not expected to create one.

`exec` also chooses its own sandbox, and `HostPlatform` can refuse an admitted call. Both make
`full-access` mean less than its label.

## Decision

### D1. The sandbox is a user setting, off by default

`/settings` has an Execution row with Sandbox and Network switches. Both are off by default; the
network switch applies only when the sandbox is on.

- Off: `exec` and the persistent shell run on the host with the user's environment, home and
  network.
- On: they run in the workspace baseline plus the platform's system roots, with the existing
  platform defaults (the persistent shell output directory; on macOS the per-process temporary
  directory and git's global configuration).
- On a platform with no sandbox backend (Windows, or Linux without `bwrap`), the next process
  start fails at preparation as `isolation_unavailable`; the Execution row shows the reason and
  the user leaves it off or installs the backend.

The setting is user configuration, not a policy file. The storage and management of the setting
are a follow-up ([#691](https://github.com/arcships/dim/issues/691)); this decision fixes its
default and its user surface.

An existing `host-isolation.json` is not read. A startup diagnostic shown while the file exists
says it is no longer used and the sandbox is off. No migration is kept.

### D2. `exec` does not choose the environment

`exec` has no `sandbox`, `paths` or `network` input. A call that still sends `sandbox` is
rejected as `exec_invalid_request`, not ignored. The model cannot widen the sandbox by calling a
tool. A command that hits a missing path fails as an ordinary command error; the user approves
the path or turns the sandbox off, then the model may retry.

### D3. `full-access` does not change the sandbox

`full-access` means “no tool prompts”. It does not turn the sandbox off, does not add paths and
does not change the network. The mode itself does not change the switch; the user changes it in
`/settings`.

File access is otherwise unchanged. Reads and writes keep the existing permission rules and the
session-file ownership rows in `session-files.md` §11.1; §11.2 is the requirement the follow-up
ADR 014 satisfies. This decision adds no grants, facts or path rules.

### D4. The isolation policy management is removed

`host.isolation.get/update/reset`, `isolation_policy.rs`, `IsolationPolicy` and
`host-isolation.json` are removed. The `/settings` Command isolation row is replaced by the
Sandbox switch. There is no node policy file and no `host-platform.json`.

## Consequences

- The release works without a policy file: the sandbox is off and the agent uses the user's
  environment. Isolation is opt-in.
- `exec` loses `sandbox`; the node policy file, `escalation` and the tier field are removed. A
  call that still sends `sandbox` is rejected.
- An existing `host-isolation.json` produces a startup diagnostic and is not read.
- The setting's storage and management, session grants, command-output ownership, the file
  access table and the owner-document updates are separate follow-ups:
  [#691](https://github.com/arcships/dim/issues/691).
- ADR 009 updates AGENTS.md rule 11's permission-preset clause. This decision does not change
  file-root authorization.

## Non-goals

- The Settings source, schema and management method for the sandbox switch.
- Session grants, command-output ownership and a new read/write table.
- A policy file, `host-platform.json`, `host-isolation.json` or a node policy document.
- Migration from the old isolation file.

## Supersession

Replaces the default-on sandbox policy, the policy file and the management methods in the
[F5 isolation design](../design/host-platform-isolation.md). It extends
[ADR 009](009-permission-decides.md); the two decisions land together. ADR 003 remains as
written.

## Rule check

| Rule | Application |
| --- | --- |
| §1, ADR 002 D1 | `Permission` and `Approval` remain branch Capabilities; `ToolExecutor` and `ToolBinding` remain branch parts; tool owners keep execution; `HostPlatform` remains a process part. No part moves. |
| §4 rule 1 | `approval/request` and `approval/respond` keep instructing the owner of admission. |
| §4 rule 2 | The admitted call is handed to execution once; the setting applies to the next process. |
| §4 rule 3 | No `before` callback is added. |
| §4 rule 4 | No core mechanism changes. |
| §4 rule 5 | No new Timeline batch. |
| §4 rule 6 | No context-input change. |
| §4 rule 7 | Parts interact through existing services. |
| §4 rule 8 | No infrastructure change; no requirement is raised to the runtime. |
| §4 rule 9 | No new Timeline fact. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | Dependencies are unchanged. |
| ADR 002 D4 | The sandbox setting is user configuration, read at process start. |
| ADR 002 D5 | No new contract in this decision; the switch's storage is a follow-up. |
