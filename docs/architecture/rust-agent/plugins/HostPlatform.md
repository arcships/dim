# HostPlatform

Owner: [host_platform.rs](../../../../crates/dim-agent-capabilities/src/host_platform.rs).
Resolves host home/environment and execution shell and supplies process spawning
and tree-supervision primitives to local tools.

## Dependencies

| Service | Purpose |
| --- | --- |
| `P:ExecutionSettingsHost` | Read the user's `Sandbox`/`Network` switches at each process preparation. Required. |
| `P:FileStorage` | Read the configured exec shell; enabled only when `exec_shell_file` is configured. |

## Events and lifetime

No Timeline events or plugin background tasks. `apply` captures the process
environment, reads the initial shell snapshot, retains the configured
FileStorage source and requires the execution-settings port. `exec_shell()`
rereads that source on every invocation.
Available resolutions and `EXEC_SHELL_CONFIG_INVALID` errors are reused while
the read result is unchanged; `EXEC_SHELL_UNAVAILABLE` is resolved again on the
next call, so transient detection failures and newly available shells recover
without editing the preference or reloading. One async mutex serializes reads
and resolution across callers and cloned handles. Unchanged available selections
do not repeat Windows shell probes. Unix shell discovery runs on a blocking
worker. Changing an available selection requires a preference edit or
HostPlatform reload. Missing files select automatically;
malformed or unreadable files return `EXEC_SHELL_CONFIG_INVALID`; an unavailable explicit
shell returns `EXEC_SHELL_UNAVAILABLE` without fallback. Resolution uses the
captured service environment, never a caller's environment.

When no shell file is configured, `exec_shell()` returns the supplied initial snapshot.
FileStorage is disabled when `exec_shell_file` is not configured.
`HostPlatform::current`
constructs such a platform, with public facts that hosts/tests may customize;
the preference source and shell snapshot are private. A value that runs commands
also needs `with_execution_settings`, which the plugin always calls with the
provided port. Callers use `exec_shell()` rather than mutating a snapshot.
`persistent_shell().await` reads
the current preference before creating a persistent shell and keeps its existing
supported-shell fallback rules. An already running shell is not restarted when
the exec preference changes. Spawned process lifetime belongs to each
caller/supervisor, not this plugin. An unavailable shell does not prevent publishing HostPlatform; exec
fails explicitly. Raw spawn/PTY and tree termination are execution mechanics;
model consumers prepare the process boundary before calling them. A
process group id equals the root pid and stays reserved only while the group
has members. Dropping a `ProcessTree` signals only when the tree was neither
confirmed terminated nor reaped on its own; otherwise a recycled pid or reused
group could receive `SIGKILL`.

`exec_root()` is `<home>/.dim/exec`, the persistent shell's own directory.
`remove_exec_directory(path)` removes one directory below it and syncs the
parent on Unix; it never reads ownership rows. SessionStore calls it during a
delete after deciding with `session_output_owners` that no Session still needs
the directory (ADR 013 D4). An absent directory counts as removed.

`read_exec_shell_settings(cancel)` reads the current version-1 preference and
detected shell choices, returning the byte revision, optional explicit executable
and any parse error. `update_exec_shell_settings(expected_revision, executable_path,
cancel)` validates the selection with the same resolver used by execution, then
replaces the preference atomically under a FileStorage lease if its byte revision
still matches. An absent executable selects automatic detection; a malformed
preference can be repaired using its current revision. Missing files have revision
`absent`. App Server exposes these operations as trusted local management methods
`host.exec_shell.get` and `host.exec_shell.update`, never model tools. No service,
dependency or assembly changes are needed for these owner operations.

The [process isolation design](../../../design/host-platform-isolation.md) owns the boundary
contract: the two switches, their defaults, each platform's roots, the temporary
directory and the failure note. `HostPlatform::current` captures the fixed platform-roots list
as `platform_roots` when the platform mounts; the sandbox builder filters it to the roots
installed on this host, and the `FileAccessFacts` service reads the same list, so the system fact
is non-empty with the sandbox off. The effective developer directory and git's global
configuration are added by the builder per process and are not facts.
`prepare_isolated_process(spec, workspace, output_dir)` reads
the switches through the port at every call, selects the existing environment values and wraps
the argv in Seatbelt or bubblewrap. The immutable Session workspace, not the command cwd,
supplies default writes; ShellSession also supplies its owned output directory, WorkspaceTools
supplies none. On macOS, default writes also include a fresh `dim-isolated-<uuid>` directory
(mode 0700) under the service's system temporary directory, which becomes TMPDIR. Preparation
returns it as a `TemporaryDirectory`; the consumer passes it to `ProcessTree::hold` or
`PtyProcessTree::hold`, and dropping the tree handle removes it on a blocking worker, so a
background handoff keeps it. Failure to create it returns `isolation_unavailable`. A sandboxed
spec also returns the crate-local `Isolation` marker, which supplies the failure note. With the
sandbox off, the original spec comes back with neither value.

A missing port or an unreadable source fails preparation as `isolation_config_invalid` before any
payload starts; it never runs unsandboxed. Sandbox on without a backend fails as
`isolation_unavailable`: always on Windows, and on Linux when bubblewrap is not on the captured
`PATH`, whose reason says to install bubblewrap or select Off in `/permissions > Process isolation`
(also available in `/settings > Process isolation`).
`HostPlatform::sandbox_backend()` reports the same decision before a process starts: `Available`,
or `Unavailable` with that reason. On Linux it uses the same captured-`PATH` bubblewrap resolution
as preparation. The App Server's `ExecutionBackend` part exposes it as `host.execution.backend`;
the TUI's `Process isolation` control and the legacy Web Execution rows display the reason. The availability report does not replace
the preparation failure. The method is a separate part because `ExecutionSettings` provides the
`ExecutionSettingsHost` port that `HostPlatform` reads; adding `HostPlatform` to
`ExecutionSettings` would close a dependency cycle.
There is no policy file, no tier and no fallback. Ordinary user ShellInput does not call this
operation. No process, task, lock, watcher or Timeline fact is added by preparation.

The crate-local `ISOLATION_SUMMARY` is the static declaration line, `None` on Windows; on macOS it
also names `$TMPDIR` as writable. It says the user decides in `/permissions > Process isolation` whether
commands run sandboxed, what the sandbox blocks, and that the model cannot change it. The exec and
persistent-shell execute declarations add it after their environment line. It does not read the
switches: declarations are read once, while each process start reads them.
`Isolation::failure_hint(output)` returns a note when the output shows a typical denial; it names
`Process isolation` in `/permissions` and distinguishes the sandbox from Tool approval.

For a persistent shell, background PIDs enter through the
existing PTY marker. `PtyProcessTree::job_pid` uses portable-pty's foreground-group handle and
Linux's namespace/session metadata to resolve the native host identity once. ShellSession stores
the typed PID or the I/O failure and uses it directly on stop or shell exit. A bounded native
stop waits for release before the cancelled terminal fact; the public signal helper is unchanged.
A running shell keeps the boundary it started with; changing the switches applies to the next
process. macOS permits only the needed terminal ioctl commands.

## Command construction

`HostPlatform::build_process_spec(command, cwd).await` reads the current shell
itself and returns `(ProcessSpec, ExecShellRuntime)`. The returned runtime is the
same selection used to build the process, so callers can apply shell-specific
policies without another read. `cwd` is an already resolved working directory.
The builder preserves shell flags, forced environment overrides and runtime PATH
prefixes, including Git Bash mapping. It does not start the user's command.

`spawn` closes the child's standard input. `spawn_with_stdin(spec, bytes)`
starts the same tree and writes `bytes` to that input from a task of the
caller's runtime, then closes it; a child that exits without reading the bytes
does not fail the start. Hooks is its consumer.

Refusals are `ProcessSpecError { code, reason }` data: malformed/unreadable
preferences (`EXEC_SHELL_CONFIG_INVALID`), unavailable shells
(`EXEC_SHELL_UNAVAILABLE`), a network-share cwd with cmd
(`EXEC_SHELL_CWD_UNSUPPORTED`), a missing cwd (`cwd_not_found`), or failed Git Bash
path mapping (`EXEC_SHELL_PATH_MAPPING_FAILED`).

## Rule check

- §1 and ADR 002 D1–D3: process facts and process preparation remain owned by HostPlatform.
  The switches belong to the App Server's `ExecutionSettings` part and reach this part
  through one declared port; no second settings store or wiring is added.
- §4 rules 1–6 and 9: no commands, handovers, before callbacks, batches or Timeline facts are
  added. Existing command execution and input adoption remain unchanged.
- §4 rule 7: callers use the current-shell methods, command builder and `spawn()`; file access
  stays behind the declared FileStorage service and the switches behind the declared port.
- ADR 002 D4: read business configuration at use time, without watchers or reload.
- ADR 002 D5: the native binding returns explicit command/shell and code/reason data. Hosts
  construct fixed platforms with `HostPlatform::current`; there is no mutable public shell
  snapshot or caller-supplied builder shell.

## Requirements raised to mechanism owners

None (§4 rule 8). Existing FileStorage, the settings port and process primitives cover the
boundary.

## Tests

[host_platform_flow.rs](../../../../crates/dim-agent-capabilities/tests/host_platform_flow.rs):
`shell_resolution_follows_the_preference_and_never_fails_the_service`,
`spawn_isolates_the_environment_and_terminate_releases_the_tree`,
`spawn_with_stdin_delivers_the_bytes_and_closes_the_pipe`,
`unchanged_shell_preferences_reuse_detection_and_edits_recover`, and
`sandbox_backend_reports_the_bubblewrap_preparation_uses`.
The unit test `bubblewrap_binds_file_roots_after_the_directories_that_contain_them` in
[isolation.rs](../../../../crates/dim-agent-capabilities/src/host_platform/isolation.rs) checks
that file roots are bound after the directories that contain them;
`bubblewrap_unshares_the_network_only_when_the_switch_is_off` (Linux) and
`seatbelt_allows_network_only_when_the_switch_is_on` (macOS) cover the platform's network
flag binding.
[local_execution.rs](../../../../tests/integration/tests/local_execution.rs):
`the_local_execution_assembly_passes_the_assembly_check` and
`exec_and_external_shell_callers_use_live_shell_preferences` exercise the real
plugin, configuration edits, refusals, recovery and the shared builder alongside exec.
[local_product.rs](../../../../tests/integration/tests/local_product.rs):
`a_turn_reads_writes_and_runs_an_approved_command_with_real_tools` checks that every request
declares exec with the same boundary line.
[minimal_execution.rs](../../../../tests/integration/tests/minimal_execution.rs):
`new_persistent_shells_and_environment_refresh_use_current_preferences` covers
first spawn after an edit, adopted environment refresh, retention of a running
shell and current selection after shell exit. `foreground_echo_returns_output` checks the execute
declaration's boundary line.
[execution_settings.rs](../../../../crates/dim-app-server/tests/transport_flow/execution_settings.rs):
`execution_settings_decide_the_next_process_boundary` writes the switches over the client
methods, checks the backend report against the platform, covers an unreadable source failing
preparation, then runs a sandboxed command with Network off and on that cannot write outside the
workspace, and the same command with the sandbox off.

<!-- generated:plugin HostPlatform — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 进程 |
| 注入 | `P:ExecutionSettingsHost`、`P:FileStorage` |
| 提供 | `P:HostPlatform` |
| 被注入于 | `HooksSession`、`PromptSources`、`ShellInput`、`SkillsCatalog`、`WorkspaceTools` |
<!-- /generated -->
