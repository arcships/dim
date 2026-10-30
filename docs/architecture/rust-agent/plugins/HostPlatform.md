# HostPlatform

Owner: [host_platform.rs](../../../../crates/dim-agent-capabilities/src/host_platform.rs).
Resolves host home/environment and execution shell and supplies process spawning
and tree-supervision primitives to local tools.

## Dependencies

| Service | Purpose |
| --- | --- |
| `P:FileStorage` | Read configured shell or isolation policy; enabled when either supported file source is configured. |

## Events and lifetime

No Timeline events or plugin background tasks. `apply` captures the process
environment, reads the initial shell snapshot and retains the configured
FileStorage source. `exec_shell()` rereads that source on every invocation.
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
FileStorage is disabled only when neither supported file source is configured.
`HostPlatform::current`
constructs such a platform, with public facts that hosts/tests may customize;
the preference source and shell snapshot are private. Callers use
`exec_shell()` rather than mutating a snapshot. `persistent_shell().await` reads
the current preference before creating a persistent shell and keeps its existing
supported-shell fallback rules. An already running shell is not restarted when
the exec preference changes. Spawned process lifetime belongs to each
caller/supervisor, not this plugin. An unavailable shell does not prevent publishing HostPlatform; exec
fails explicitly. Raw spawn/PTY and tree termination are execution mechanics;
model consumers prepare the local isolation policy before calling them. A
process group id equals the root pid and stays reserved only while the group
has members. Dropping a `ProcessTree` signals only when the tree was neither
confirmed terminated nor reaped on its own; otherwise a recycled pid or reused
group could receive `SIGKILL`.

The [F5 isolation spike](../../../design/host-platform-isolation-spike.md) exercises
Seatbelt, Landlock and bubblewrap wrappers through these existing process and PTY APIs. It
does not establish product acceptance. Host-local policy and native consumer acceptance are
specified in the [F5 product design](../../../design/host-platform-isolation.md).
The policy is configurable and enabled by default for model exec/background work and
persistent shells on macOS/Linux. A missing file uses system-read/workspace-write defaults,
plus the host-owned current branch output directory for persistent shells. On macOS the system
reads include the `/var/select` links, the effective developer directory (DEVELOPER_DIR,
which passes by default, or the linked selection) and the files of git's global configuration
with their canonical paths. Every granted root's or file's ancestors permit metadata lookups,
and each process tree gets its own TMPDIR. The product design lists each platform's defaults.
Missing policies and version 2 policies omitting escalation use `ask`; explicit `deny` and
version 1 retain their restrictive contracts. The [product design](../../../design/host-platform-isolation.md#trusted-local-policy-management)
defines typed policy read/update/reset operations owned here, with revisions and atomic writes
through FileStorage. `read_isolation_policy(cancel)` returns the shared
`dim_agent_types::IsolationPolicySnapshot`: actual path, byte revision, source, editable policy
and effective boundary. `update_isolation_policy(expected_revision, policy, cancel)` validates
external fields and replaces the file under a FileStorage lease only when the revision matches.
`reset_isolation_policy(expected_revision, cancel)` writes explicit v2 product defaults.
`IsolationPolicyError` distinguishes invalid policy, revision conflict, unavailable management
and storage failure. App Server registers their trusted local management entry points; the CLI
uses them from `/settings > Command isolation`, never a model tool. Local configuration can
change roots, network/environment and disable isolation. User ShellInput
commands remain ordinary spawn. BackgroundExec retains its existing live process/tree; no
namespace PID registry is introduced. Unsupported Windows preserves ordinary execution.

`HostPlatformConfig.isolation_file` identifies node-local `host-isolation.json` through
FileStorage. Version 2 accepts sandbox, escalation, readOnlyPaths, writablePaths and environment;
version 1 keeps enabled and network. The product design defines defaults, tiers and empty-list
semantics. readOnlyPaths names existing absolute directories or regular files, writablePaths only
directories. Unknown fields/versions, invalid roots or environment names and unreadable files
return isolation_config_invalid. Missing files use defaults without writing a file. Windows does
not load this unsupported source. Local edits affect the next model process, retaining the policy
of an existing shell.
Linux without bwrap on the captured PATH returns isolation_unavailable; its reason says to
install bubblewrap or set `"sandbox": "host"` in the policy file, named at its location.

The crate-local `prepare_isolated_process(spec, workspace, output_dir, requested)` operation reads and validates the
external file once, resolves the tier from the node policy and the call's optional `sandbox`
request, selects existing environment values and wraps the argv in Seatbelt or
bubblewrap. A request above the node tier without `escalation: "ask"` returns
`isolation_denied` before any payload starts; a narrower one always applies. A file root becomes a Seatbelt `literal` rule, or a read-only bubblewrap bind after
the read-only directories and before the writable ones. The immutable Session workspace, not
the command cwd, supplies default writes.
ShellSession also supplies its owned output directory; WorkspaceTools supplies none. On macOS,
default writes also include a fresh `dim-isolated-<uuid>` directory (mode 0700) under the service's
system temporary directory, which becomes TMPDIR. Preparation returns it as a `TemporaryDirectory`;
the consumer passes it to `ProcessTree::hold` or `PtyProcessTree::hold`, and dropping the tree handle
removes it on a blocking worker, so a background handoff keeps it. Explicit
writablePaths fully replaces these defaults, including an empty list granting no directory writes,
and then no TMPDIR is created or set. Failure to create it returns `isolation_unavailable`.
Disabled policy and Windows return the original spec, no `TemporaryDirectory` and no `Isolation`.
A sandboxed spec also returns the crate-local `Isolation`, which records where the user changes
the policy: the configured FileStorage root joined with the file's relative path, or the file
name without a location. `Isolation::failure_hint(output)` returns a note when the output shows a
typical denial; it names that location and `/settings > Command isolation`, and distinguishes
isolation from command approval. Ordinary user ShellInput does not call this operation. No
process, task, lock, watcher or Timeline fact is added by preparation.
The crate-local `ISOLATION_SUMMARY` is the static declaration line on default isolation, `None`
on Windows; on macOS it also names `$TMPDIR` as writable, and so does the failure note. The exec
and persistent-shell execute declarations add it after their environment line. It does not read
the policy: declarations are read once, while each process start reads it.

For a persistent shell, background PIDs enter through the
existing PTY marker. `PtyProcessTree::job_pid` uses portable-pty's foreground-group handle and
Linux's namespace/session metadata to resolve the native host identity once. ShellSession stores
the typed PID or the I/O failure and uses it directly on stop or shell exit. A bounded native
stop waits for release before the cancelled terminal fact; the public signal helper is unchanged.
The actual model flow verifies the selected job's TERM cleanup before stop replies, sibling
survival, Session-close cleanup and exactly one terminal fact with isolation enabled or disabled.
The same flow edits local policy while the shell runs and verifies that its startup filesystem
boundary remains in force, with the ungranted home file readable only in the disabled control.
It also checks a real terminal, window-size ioctls, occupying-command stop and the next usable
shell call. Existing stop behavior may reset the shell if no completion marker arrives.
macOS permits only the needed terminal ioctl commands. Enabled networking permits the system
DNS resolver socket; other UNIX sockets remain restricted to granted file trees. The native
socket flow verifies hostname resolution to a live local listener, denied ungranted sockets
and actual delivery after an explicit path grant, alongside IPv4/IPv6 TCP/UDP controls.

## Command construction

`HostPlatform::build_process_spec(command, cwd).await` reads the current shell
itself and returns `(ProcessSpec, ExecShellRuntime)`. The returned runtime is the
same selection used to build the process, so callers can apply shell-specific
policies without another read. `cwd` is an already resolved working directory.
The builder preserves shell flags, forced environment overrides and runtime PATH
prefixes, including Git Bash mapping. It does not start the user's command.

Refusals are `ProcessSpecError { code, reason }` data: malformed/unreadable
preferences (`EXEC_SHELL_CONFIG_INVALID`), unavailable shells
(`EXEC_SHELL_UNAVAILABLE`), a network-share cwd with cmd
(`EXEC_SHELL_CWD_UNSUPPORTED`), a missing cwd (`cwd_not_found`), or failed Git Bash
path mapping (`EXEC_SHELL_PATH_MAPPING_FAILED`).

## Rule check

- §1 and ADR 002 D1–D3: process facts and policy validation/persistence remain owned by
  HostPlatform. App Server's process management entry depends on HostPlatform and
  managementMethods only; it exposes the owner's operations without a second policy store.
- §4 rules 1–6 and 9: no commands, handovers, before callbacks, batches or Timeline
  facts are added. Existing command execution and input adoption remain unchanged.
- §4 rule 7: callers use the current-shell methods, command builder and `spawn()`;
  file access stays behind the declared FileStorage service.
- ADR 002 D4: read business configuration at use time, without watchers or reload.
- ADR 002 D5: the native binding returns explicit command/shell and code/reason
  data. Hosts construct fixed platforms with `HostPlatform::current`; there is no
  mutable public shell snapshot or caller-supplied builder shell.

## Requirements raised to mechanism owners

None (§4 rule 8). Existing FileStorage and process primitives cover S7 and F5.

## Tests

[host_platform_flow.rs](../../../../crates/dim-agent-capabilities/tests/host_platform_flow.rs):
`shell_resolution_follows_the_preference_and_never_fails_the_service`,
`spawn_isolates_the_environment_and_terminate_releases_the_tree`,
`unchanged_shell_preferences_reuse_detection_and_edits_recover`.
[local_execution.rs](../../../../tests/integration/tests/local_execution.rs):
`exec_and_external_shell_callers_use_live_shell_preferences` exercises the real
plugin, configuration edits, refusals, recovery and the shared builder alongside exec.
`model_exec_isolation_reads_local_policy_without_expanding_from_command_cwd` covers file
grants, writable children of read-only roots, refusal of read-only children of writable roots
(including canonical sources and declared mount destinations), explicit read-only roots with empty writablePaths, preserved bytes/mode after denied
writes/chmod, unreadable or invalid policy refusal (including missing, relative and
non-regular read-only roots, a file root inside a writable root and a file in writablePaths) and
no fallback when runtime reads are ungranted, and that explicit writablePaths leave TMPDIR unset.
`explicit_file_roots_let_git_read_its_configuration_but_not_their_siblings` commits with the
identity of a plain or symlinked `~/.gitconfig` that an explicit list names as a file root,
keeps that file read-only and keeps credential files beside the link and its target denied.
CI has no Linux job, so the unit test
`bubblewrap_binds_file_roots_after_the_directories_that_contain_them` in
[isolation.rs](../../../../crates/dim-agent-capabilities/src/host_platform/isolation.rs) checks
the bind order.
`default_macos_isolation_runs_developer_tools_without_host_tmp_or_home` runs git and python3
through the `/usr/bin` shims without a policy file, with DEVELOPER_DIR naming the Command Line
Tools wherever they are installed, writes temporary files through per-process
TMPDIRs that survive the background handoff and are removed with their trees, and keeps /tmp,
the service's temporary directory and home listing denied.
`default_macos_isolation_reads_git_global_configuration_but_not_other_home_files` commits with
the identity and alias of a plain or symlinked `~/.gitconfig`, honors the XDG excludes file under
HOME or a moved XDG_CONFIG_HOME, and keeps credential files, `~/.ssh` and the home listing
denied. `an_unavailable_shell_or_backend_refuses_the_payload` also verifies refusal before
admission when Linux has no bubblewrap on the captured PATH, with a reason that says to install
bubblewrap or set `"sandbox": "host"`. `local_isolation_policy_controls_native_sockets` covers
live IP and UNIX listeners under the local policy.
`isolated_exec_failures_point_to_the_local_policy` checks the failure note for a denied write
in the foreground and background, a resolver failure and an explicit read-only workspace, and
its absence for an ordinary failure, a success and disabled policy.
[local_product.rs](../../../../tests/integration/tests/local_product.rs):
`a_turn_reads_writes_and_runs_an_approved_command_with_real_tools` checks that every request
declares exec with the same isolation line on macOS/Linux.
[minimal_execution.rs](../../../../tests/integration/tests/minimal_execution.rs):
`new_persistent_shells_and_environment_refresh_use_current_preferences` covers
first spawn after an edit, adopted environment refresh, retention of a running
shell and current selection after shell exit.
`persistent_isolation_preserves_policy_and_job_control` covers the actual model path with
default and explicit local grants, startup-policy retention, selected-job stop, TERM cleanup,
sibling survival and Session close. It verifies sibling output denial under defaults and
refuses background payloads when explicit writable roots omit the shell's output directory,
including an empty list. Under defaults the shell's TMPDIR accepts a temporary file and is removed
after Session close. `foreground_echo_returns_output` checks the execute declaration's
isolation line.

<!-- generated:plugin HostPlatform — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 进程 |
| 注入 | `P:FileStorage` |
| 提供 | `P:HostPlatform` |
| 被注入于 | `PromptSources`、`ShellInput`、`SkillsCatalog`、`WorkspaceTools` |
<!-- /generated -->
