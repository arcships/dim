# HostPlatform process isolation

The sandbox is a user setting, and `HostPlatform` is the only code that applies it
([ADR 010](../adr/010-sandbox-opt-in.md), [ADR 011](../adr/011-execution-settings.md)). Permission
decides whether a call runs ([ADR 009](../adr/009-permission-decides.md)); neither a preset, an
approval nor the model changes the boundary. There is no node policy file and no escalation. A
daemon that finds the `host-isolation.json` of earlier releases names it on stderr at startup and
asks the user to delete it; it never reads it.

## Scope

The switch covers WorkspaceTools `exec`, its BackgroundExec handoff, and persistent ShellSession
creation. User-typed `!` / `!!` commands belong to ShellInput and use ordinary spawn; they are
outside this boundary ([ADR 003 D1](../adr/003-approval-scope.md)). The App Server, model
connections, MCP servers and trusted plugins are outside it. Hooks and MCP commands run because of
configuration and need no host-local approval ([ADR 003](../adr/003-approval-scope.md)).

## Process isolation

The TUI's single `Process isolation` control replaces `Settings > Execution` and any separate
Sandbox entry ([ADR 017](../adr/017-independent-permission-controls.md)). It reuses the existing
backend and stored `sandbox`/`network` booleans; it does not introduce another isolation policy.

| Choice | Stored sandbox / network | Boundary |
| --- | --- | --- |
| Off | false / network ignored | The daemon user's ordinary access: environment, home and network |
| Sandboxed · No network | true / false | The Session workspace writable, system roots read-only, no network |
| Sandboxed · Network allowed | true / true | The same files, plus host networking |

```text
Process isolation
> Off
  Sandboxed · No network
  Sandboxed · Network allowed
  Applies to new processes and persistent shells.
```

Both stored booleans default to false. Network applies only while the sandbox is on. Selecting
Off disables `sandbox`; an existing `network` value is inert while off. Each sandboxed UI choice
sets `network` explicitly; local management clients still use the stored booleans directly.
The two values are user-scope configuration owned by
the App Server's `ExecutionSettings` part; its source, schema and CAS revision are
[settings service](appserver/settings-service.md)'s contract. A write to that source is a
user-approved write ([ADR 011 D2](../adr/011-execution-settings.md)).

## Call flow

1. WorkspaceTools builds its existing `ProcessSpec`; ShellSession selects its existing persistent
   shell. The Session configuration supplies the workspace separately from the model's requested
   working directory. `exec` has no input that selects a boundary; a call that still sends
   `sandbox` is rejected as `exec_invalid_request`.
2. `HostPlatform::prepare_isolated_process(spec, workspace: &Path, output_dir: Option<&Path>)`
   reads the execution switches through the `ExecutionSettingsHost` port, selects the environment
   names and constructs the backend argv or profile. Only ShellSession supplies its immutable,
   host-owned current branch output directory. It returns the prepared spec, the process's
   [temporary directory](#temporary-directory) when one applies, and the crate-local `Isolation`
   marker the consumer keeps for its [failure note](#failure-note).
3. The consumer calls the existing synchronous spawn or spawn PTY and attaches the temporary
   directory to the returned tree. Backend setup precedes payload execution; a failed setup runs no
   payload. Arguments and paths stay separate argv values.
4. Existing process/PTY trees, pumps, cancellation and terminal facts remain with their current
   owners.

Raw `HostPlatform` spawn and spawn PTY remain available for ShellInput and trusted host uses. The
switches are read at each process preparation, so a running process, including a persistent shell,
keeps the boundary it started with. Shell and runtime discovery stay outside the sandbox.

## Platform bindings

`HostPlatform` captures the fixed platform-roots list below when it mounts. The sandbox binds
the installed roots, and the `FileAccessFacts` service reads the same list as its system fact
([ADR 014](../adr/014-file-access-facts.md) D1), so the fact holds with the sandbox off. The
developer directory and git's global configuration files are per-process additions and are not
facts.

### macOS

`/usr/bin/sandbox-exec` with a `HostPlatform`-generated Seatbelt profile: deny by default, allow
reads only for the granted roots and required runtime devices, writes only for the workspace, the
shell output directory and the [temporary directory](#temporary-directory), and deny networking
unless Network is on. Enabled network permits IP operations and the system DNS resolver socket at
`/private/var/run/mDNSResponder`, with metadata access to its `/var` alias. Other UNIX-socket
connections stay limited to the granted trees, with local socket binding limited to writable
trees. Profile parameters carry host paths; no path is embedded unescaped into policy source.

A directory root becomes a `subpath` rule and a file root a `literal` rule, each at its canonical
source and, when that differs, its requested destination. Each granted root also permits metadata
lookups on its path ancestors, as Apple's system profile does for firmlink components: `stat` of
those directories works, listing and reading them do not. `realpath(3)` examines every component,
so Python and Clang cannot locate themselves without it. Inherited PTYs permit only terminal-mode,
geometry and foreground-group ioctl commands on tty devices.

The default read-only roots are `/System`, `/usr`, `/bin`, `/sbin`, `/Library/Apple`,
`/private/etc`, `/etc`, `/var/select` and the effective developer directory: `DEVELOPER_DIR` when
the command's environment sets it, otherwise what `/var/select/developer_dir` selects. The
`/usr/bin` shims (`git`, `python3`, `clang`, `make`) run the tools in that directory, so the grant
follows `xcode-select` rather than a fixed install location. An Xcode.app selection is not part of
the supported baseline: the shims load frameworks from the bundle outside `Contents/Developer`.
Such a host selects the Command Line Tools with `xcode-select -s /Library/Developer/CommandLineTools`,
starts dimcode with `DEVELOPER_DIR` naming them, or leaves the sandbox off. `HostPlatform` never
substitutes the Command Line Tools for the user's selection.

Git exits when its global configuration exists but cannot be read, and even `git --version` reads
it, so the default grants read access to the files git reads for that configuration, each only
where it exists: `$HOME/.gitconfig`, and `config`, `ignore` and `attributes` in the XDG git
directory (`$XDG_CONFIG_HOME/git`, or `$HOME/.config/git`). Each is a single-file grant, plus its
canonical path when a dotfile manager makes it a symlink. The XDG directory is granted file by
file, not as a subtree, because git's credential store also reads `credentials` there.
`~/.git-credentials`, the XDG `credentials` file, `~/.ssh` and every other home path stay
ungranted. `include.path` and `includeIf` targets elsewhere in the home are not granted, so git
fails with `Operation not permitted`; a `core.excludesFile` or `core.attributesFile` elsewhere
draws a warning and is not applied. Reads of the home are not the model's secret boundary: the
`read` tool asks the user for a home path through [ADR 014's facts](../adr/014-file-access-facts.md)
rather than relying on the sandbox
([session files §11.2](appserver/session-files.md#112-requirement-for-path-scoped-file-access)),
and this is a narrow tool-configuration grant.

The included environment names are `PATH`, `HOME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TERM`,
`NO_COLOR`, `PAGER`, `GIT_PAGER`, `GH_PAGER`, `DEVELOPER_DIR` and `XDG_CONFIG_HOME`; everything
else the caller set is dropped. `sandbox-exec(1)` is marked deprecated upstream; support is
limited to OS versions that pass the isolation acceptance flows.

### Linux

bubblewrap with an empty mount namespace, private user, PID, IPC and UTS namespaces, private
`/proc`, minimal `/dev`, `--die-with-parent`, and payload capabilities dropped. Without Network,
the network namespace is unshared; with Network it is the host's. bwrap is resolved through the
captured host `PATH`; the host must trust that `PATH` and the selected executable, including its
writable ancestors. No provenance check, download or elevation is promised.

The default read-only roots are `/usr`, `/bin`, `/sbin`, `/lib`, `/lib64` and `/etc`; writable
roots are the Session workspace, the shell output directory and nothing else. Each root is bound
from its canonical source at its requested destination, and read-only directories come first so a
later writable bind can lie inside one. Retain the existing ProcessTree and PtyProcessTree; do not
add `--new-session` to the PTY path, where measured job control broke. No terminal proxy, seccomp
system or process supervisor is added.

The namespace has no `/tmp` and receives no TMPDIR. The default mounts exclude the home, so git
finds no global configuration here and runs without one. Resolver setups whose `/etc/resolv.conf`
points into `/run/systemd/resolve`, or whose NSS module needs a socket there, are not supported by
the default mounts. Landlock is not selected: its measured ABI 4 wrapper covers neither UDP/UNIX
sockets nor all read-only metadata operations. See the upstream
[policy limits](https://github.com/containers/bubblewrap/blob/main/README.md) and
[options](https://github.com/containers/bubblewrap/blob/main/bwrap.xml).

### Windows

There is no sandbox backend. Preparation reads the switches and, with Sandbox on, fails as
`isolation_unavailable`; the payload never runs on the host as if it were sandboxed. Path checks and
Permission rules never stand in for the backend, and the missing backend is reported rather than
simulated.

### Background lifetime

The existing BackgroundExec owns model background tasks. `exec::hand_off` passes the same live
process to `BackgroundExec::start`, and its task retains the tree and its startup boundary until
completion. Returning a tool result or stopping a turn does not drop that process.

| Transition | Owner contract |
| --- | --- |
| Foreground becomes background | Move the live process and tree; retain the same namespace. |
| Turn stops | BackgroundExec keeps the task running and retains its completion/wake ownership. |
| Explicit task stop or unload | The existing task cancellation terminates the owned tree before terminal settlement. |
| Command completes normally | Record `exec/ended` once and publish the existing bounded wake. |
| App Server dies | Backend parent-death handling terminates isolated descendants; nothing is recovered from history. |

Shell backgrounding inside a command creates no BackgroundExec task. ShellSession keeps its
original PTY and tree for the shell's lifetime; the existing PTY foreground-group handle resolves
the shell's Linux PID namespace and OS session, and the private task stores a typed host PID or its
I/O error. Task control uses the existing taskId; no namespace registry, PID mapping table or
supervisor is held.

## Failure

A port that is missing or a source that cannot be read fails preparation as
`isolation_config_invalid`: the switches cannot be read, so the command does not run. Sandbox on
without a backend fails as `isolation_unavailable` — on Windows always, on Linux when bwrap is not
on the captured `PATH`. Nothing starts a payload at a weaker boundary; there is no implicit
fallback. A denied write or a refused socket reaches the command as an ordinary error.

The same backend state is readable before a process starts. `HostPlatform::sandbox_backend()`
reports `Available`, or `Unavailable` with the reason the preparation failure would use; the App
Server's `ExecutionBackend` part exposes it as `host.execution.backend` to the Process isolation control. The
Linux answer comes from the same captured-`PATH` bubblewrap resolution preparation uses, so a host
that reports the reason cannot start a different boundary. ADR 010 D1 keeps the preparation failure
as the enforcement; the row only names it earlier.

## Visibility

**Tool declarations.** On macOS and Linux, the `exec` and persistent-shell `execute` declarations
carry one static line from `HostPlatform`: the user selects Process isolation in `/permissions`
or `/settings` to decide whether
commands run sandboxed; when the sandbox is on, commands write only inside the session workspace
(on macOS also `$TMPDIR`), read only there and in system directories, and have no network unless
the user also turned Network on; the model cannot change the boundary and retrying or an approval
does not lift it. The line describes the switches, not their current value, because ToolRegistry
reads declarations when it applies or attaches a generation while each process start reads the
switches. Windows declarations carry no line.

<a id="failure-note"></a>
**Failure note.** When a sandboxed command exits unsuccessfully, in the foreground or after the
background handoff, and the output shown in its result contains a typical denial, the result text
ends with a note. It says the sandbox the user turned on may have caused the failure, what it
blocks, that retrying or an approval does not lift it, and that the user changes Process isolation
in `/permissions` or `/settings`. Typical denials are `Operation not permitted` and dyld's
`blocked by sandbox` on macOS, `Read-only file system` and `bwrap:` on Linux, resolver failures and
`Network is unreachable`. The note is a heuristic: the backends report no denial event, ungranted
Linux paths are simply absent, and a matching message can have another cause. It is text in the
existing result content; structured content, codes, Timeline facts and approval categories are
unchanged, and it offers no approval or rule. Successful commands, other failures and commands run
with the sandbox off get no note.

**User documentation.** The [CLI guide](../../crates/dim-cli/README.md#execution-boundary) carries
the user page: the switch locations, what each grants per platform, and what the sandbox does not
cover.

## Persistent shell output

ShellSession adds its host-owned current branch output directory,
`ShellSessionConfig.output_dir` (normally `HostPlatform.home/.dim/exec/<session>/<branch>`), to the
writable roots alongside the Session workspace. The shell owner creates this directory before
preparation; the model's cwd and command cannot select or expand the grant. A sibling branch's
output directory and the rest of the home receive no implicit access, apart from git's
configuration on macOS.

Existing output paths keep their owners. WorkspaceTools' `ExecProcess` pumps child output into its
host-owned output sink; an occupying ShellSession command appends its captured PTY text from
`OccupyingTask::commit`; explicit POSIX background jobs open their log inside `posix_background` and
the host reads it for previews. That log is written inside the grant above, so a sandboxed shell can
always write it and the rest of the home stays unwritable. Stdio and PTY remain runtime channels,
not grants.

## Temporary directory

On macOS, when the sandbox is on, preparation creates one empty directory for the process tree,
adds it to the writable roots and sets `TMPDIR` to it. It is created with mode 0700 and a fresh
`dim-isolated-<uuid>` name under the App Server's system temporary directory (normally the per-user
`/var/folders/…/T`). The parent directory, its other entries and `/tmp` stay ungranted. Each
WorkspaceTools exec process receives its own directory; a persistent shell receives one for its
lifetime.

The process tree handle owns the directory: the consumer attaches it to the spawned tree, and
dropping that handle removes the directory on a blocking worker. A background handoff moves the
handle, so BackgroundExec keeps the directory until it settles the task. A failed spawn removes it
at once. Descendants that outlive their reaped command root lose the directory. If the App Server
dies, the directory stays in the system temporary area for the OS's ordinary cleanup; there is no
startup sweep, registry or recovery. Failure to create the directory refuses the payload as
`isolation_unavailable`, and the message names the switch to turn off.

Linux has no `/tmp` in the namespace and receives no `TMPDIR`; the sandbox is off by default, so a
command there runs with the host's temporary directory.

## Rule check

| Rule | Application |
| --- | --- |
| §1, ADR 002 D1 | Extend the existing process owner; the switches belong to the App Server's `ExecutionSettings` part and reach `HostPlatform` through one port. No new capability, registry or supervisor. |
| §4 rule 1 | Consumer admission and results retain their existing owners. |
| §4 rule 2 | Background transfer retains the live tree and its startup boundary. |
| §4 rule 3 | No `before` callback or commit-time I/O. |
| §4 rule 4 | Core and assembly contain no isolation business branch. |
| §4 rule 5 | No new batch or dispatch semantics. |
| §4 rule 6 | Only existing tool results and adopted inputs enter model context. |
| §4 rule 7 | Public HostPlatform and tree interfaces, the declared `ExecutionSettingsHost` port, and declared FileStorage only. |
| §4 rule 8 | Existing mechanisms suffice; no requirement is raised to the runtime. |
| §4 rule 9 | The switches and diagnostics are not Timeline facts. |
| ADR 002 D2 | No wiring or cross-scope policy service. |
| ADR 002 D3 | One declared process dependency on the port. |
| ADR 002 D4 | Business configuration, read at each process start. |
| ADR 002 D5 | The port is an in-process Rust contract; clients use `settings.read`/`settings.update`. |
| ADR 003 D1 | A `sandbox` input is not part of an admitted `exec` call. |
| ADR 003 D5 | A preset and an approval confer no boundary; the user's switches alone decide it. |

## Requirements raised to mechanism owners

None.

## Ablation

Two booleans replace the policy file, its schema, its revisioned management methods and the
escalation gate. One preparation operation reuses the existing spawn and spawn PTY paths and native
errors; the port is the same shape as `CommandRulesHost` and `SkillSettingsHost`. BackgroundExec
already holds the process, so no namespace or PID manager is needed, and ShellInput stays outside
this boundary. The declaration line and the failure note are text in existing places — no new
result field, code, Timeline fact or approval category.
