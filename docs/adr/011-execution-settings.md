# 011: Execution settings

Decision status: Proposed.

Extends [ADR 010](010-sandbox-opt-in.md): the sandbox is opt-in. This decision completes the
storage and management ADR 010 defers, and qualifies its “the model cannot widen the sandbox”
sentence. ADR 003 is unchanged.

## Context

ADR 010 fixes the default, the two switches and the `/settings` surface. The switches still need
one owner, schema and read path. Main carries the old isolation policy surface; the new one must
not become another required file.

## Decision

### D1. One owner, one source, one port

The App Server registers a process part `ExecutionSettings`, next to `SettingsMethods`. From its
own `apply` it registers the built-in declared source `execution`, user scope only, with two
boolean fields: `sandbox` and `network`, both `false`. The source has the CAS revision of the
Settings service; local management clients and `/settings` use `settings.read` and
`settings.update`. It has no project file.

The same part provides the read-only host port `ExecutionSettingsHost`. `HostPlatform` declares
the port and reads it at each process preparation; a running process, including the persistent
shell, keeps the boundary it started with. A missing port or an unreadable source fails
preparation; it never runs unsandboxed.

A HostPlatform file such as `exec-shell.json` is not the owner: it holds the shell selection, not
the user-scope execution switches, and it has no CAS revision or `/settings` management. Adding
the switches there would create a second management surface for no gain.

### D2. A write to the store is a user-approved write

ADR 010 says the model cannot widen the sandbox by calling a tool. The switches are stored
configuration: a model that can write the store — under `full-access`, or through an approved
outside write under `workspace-write` — changes the value, and the next process uses it. That
write is the user's approved action, the same authority as changing the row in `/settings`; it
creates no grant and does not change a running process. A user who wants a boundary does not
approve such a write.

## Consequences

- `ExecutionSettings` owns the source and provides `ExecutionSettingsHost`; `/settings` and local
  management clients use `settings.read`/`settings.update`.
- `host.isolation.get/update/reset`, both `isolation_policy.rs` files, `IsolationPolicy`,
  `host-isolation.json` and the Command isolation row are removed by ADR 010; this decision adds
  the source and the port that replace their read and management.
- `settings-service.md`, `clients.md` and its Chinese mirror, `overview.md`,
  `crates/dim-cli/README.md`, `docs/design/cli-commands.md` and its Chinese mirror, the
  `HostPlatform.md` page, `dev`, `host_platform.rs` and the F5 design are updated under their
  owners after acceptance. `readOnlyPaths`, `writablePaths` and `environment` have no
  replacement. The change is tracked in #691.
- An existing `host-isolation.json` is not read; ADR 010's startup diagnostic is the visible
  channel, and the user removes the file.
- No Timeline event or wire field is added. A branch part appends a one-time `<dim-state>` snapshot on open and on each `SettingsChanged`, and a Session part registers the read-only `settings` tool; both are inputs and tool results, not Prompt sources. The source remains business configuration.

## Non-goals

- Session grants, command-output ownership and file-access facts.
- A project-scoped execution source.
- Migration from the old file.

## Supersession

Extends [ADR 010](010-sandbox-opt-in.md) and completes its deferred storage and management. It
qualifies ADR 010 D2 and D3 for a write to the store; the two decisions land together. ADR 003
remains as written.

## Rule check

| Rule | Application |
| --- | --- |
| §1, ADR 002 D1 | `ExecutionSettings` is an App Server process part that owns the source and the port; `HostPlatform` remains a process part and reads the port. |
| §4 rule 1 | The source is configuration with typed `settings.read/update`; no command event. |
| §4 rule 2 | The source applies to the next process; an admitted call is unchanged. |
| §4 rule 3 | No `before` callback. |
| §4 rule 4 | No core mechanism change. |
| §4 rule 5 | No Timeline batch. |
| §4 rule 6 | The snapshot enters the model as an appended input; no Prompt source and no second context path. |
| §4 rule 7 | `HostPlatform` reads its port; the App Server part reads the typed `SettingsService`. |
| §4 rule 8 | No infrastructure change; the port follows `CommandRulesHost` and `SkillSettingsHost`. |
| §4 rule 9 | No new Timeline fact. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | The source is user-scoped; `HostPlatform` depends on the port. |
| ADR 002 D4 | Business configuration, read at use time. |
| ADR 002 D5 | The client surface is `settings.read/update`; the in-process surface is `ExecutionSettingsHost`. |
