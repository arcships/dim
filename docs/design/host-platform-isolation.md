# HostPlatform process isolation

Status: implemented for [F5 #76](https://github.com/arcships/dim/issues/76), delivered
by merged [#189](https://github.com/arcships/dim/pull/189). The macOS developer-tool baseline,
git's global configuration and the per-process temporary directory are calibrated for
[#309](https://github.com/arcships/dim/issues/309). [Visibility](#visibility) follows
[#308](https://github.com/arcships/dim/issues/308). File entries in `readOnlyPaths` follow
[#323](https://github.com/arcships/dim/issues/323).
Requirements come from [overview Appendix B.1](overview.md#b1-session-and-branch-capabilities),
the [feature list](../feature-list.md), and #76's maintainer decision: configurable isolation
for model-initiated execution. The [spike report](host-platform-isolation-spike.md) supplies
mechanism evidence, not product requirements.

## Scope and policy

Isolation covers WorkspaceTools exec, its BackgroundExec handoff, and persistent ShellSession
creation. User-typed `!` / `!!` commands belong to ShellInput and use ordinary spawn; they are
outside F5 ([ADR 003 D1](../adr/003-approval-scope.md)). App Server, model connections, MCP servers
and trusted plugins are outside this process boundary. MCP isolation remains a separate
[open question](overview.md#9-open-questions).

On macOS/Linux isolation is enabled by default and can be configured or disabled locally.
Unsupported Windows preserves ordinary execution. When enabled, missing backend support or
failed setup refuses the payload; it never silently retries without isolation.

A model command runs at one of three tiers, named by the node policy:

| Tier | Boundary |
| --- | --- |
| `workspace` | The Session workspace is writable, system directories are read-only, no network (the default) |
| `network` | The same files, plus host networking |
| `host` | No process isolation: the daemon user's ordinary access |

`exec` carries an optional `sandbox` argument naming a tier; the node's tier is both the default
and the ceiling. A request at or below it applies directly. A request above it runs only where the
node sets `escalation: "ask"`, and otherwise fails with `isolation_denied` before any payload
starts. The argument is the model's decision, so Permission and Approval admit the call as usual;
neither widens anything by itself, and a permission preset is not authority (ADR 003 D5). The
approval prompt shows the call with the requested `sandbox` boundary. Under `workspace-write`
commands ask, so an escalated command is approved per call; under `read-only` they are denied;
under `full-access` they run without a prompt. A tool in `allowedTools` or a command matched by an
allow rule is admitted without a prompt at any tier, since neither looks at `sandbox`. Where no
prompt appears, `escalation: "ask"` is the only gate. The persistent shell has no argument and
keeps the node tier.

| Resource | Default and configuration |
| --- | --- |
| Files | Platform system executable/library/configuration directories are read-only; the Session workspace is writable. Persistent shells also have their host-owned current branch output directory writable by default. On macOS each isolated process tree also has its own host-owned [temporary directory](#temporary-directory) writable by default. Explicit read/write lists replace those defaults and may be empty; the read-only list may also name single files. |
| Workspace | The immutable Session `cwd`, supplied by the consumer. A model's per-command cwd never expands the grant. |
| Network | Off at the `workspace` tier; the `network` tier enables host/external networking. A wider tier is the `sandbox` argument plus the node's `escalation`, never a per-command exception outside that policy. |
| Environment | An explicit name list selects values already in ProcessSpec. Defaults are PATH, HOME, LANG, LC_ALL, LC_CTYPE, TERM, NO_COLOR, PAGER, GIT_PAGER and GH_PAGER; macOS adds DEVELOPER_DIR and XDG_CONFIG_HOME, which select the developer directory and git configuration that its default grants cover. A name passes only when the service environment sets it. When the default temporary directory applies, TMPDIR names it and replaces any selected value. No credential variables are synthesized. |
| Runtime | Backend-required devices and the caller's existing stdio/PTY; Linux has its private /proc and namespace skeleton. Host /tmp, the user's own TMPDIR and the home receive no implicit grant, apart from git's global configuration on macOS; macOS temporary files use the per-process directory above. |
| Running processes | Policy is a startup snapshot. Edits affect the next process; running exec and persistent shells retain their original boundary. |

A missing policy file uses `workspace` with `escalation: "ask"`; it does not write a file.
`ask` delegates admission to Permission/Approval, not an unconditional additional prompt:
the ordinary default interactive `workspace-write` preset asks for commands and displays the
requested boundary; `full-access`, an allowed tool or a matching allow rule can admit a wider
request without a prompt. Explicit node `deny` still refuses that request after admission.
Version 2 files omitting `escalation` also use `ask`: omission is not an explicit denial.
An explicit `escalation: "deny"` is required to refuse wider requests at the node policy gate.
Version 1 retains its legacy no-escalation contract.
The platform's default system roots are calibrated as part of its supported baseline: macOS
uses /System, /usr, /bin, /sbin, /Library/Apple and /private/etc (including its /etc alias),
plus /var/select (the root-owned selection links that /bin/sh and the /usr/bin developer-tool
shims read, including its /private/var/select source), the effective developer directory and
git's global configuration (see [macOS](#macos)); Linux uses its installed system
executable, loader/library and /etc roots (/usr, /bin, /sbin, /lib, /lib64 and /etc), including
the needed absolute-path aliases. Optional
architecture-specific system directories, the developer directory and git's configuration are
included only where they exist. Apart from git's configuration files, these are platform
runtime grants, not an implicit read grant to the user's home. Additional toolchains (Homebrew,
rustup, nvm and similar), caches or downloads may require local configuration. The default must pass a
real system-shell/workspace read/write flow on each supported baseline; on macOS that flow
includes the developer tools (git with the user's global configuration, python3) with the Command
Line Tools selected, and a temporary file.

This isolates process file/network access, not trusted plugins, all tools, CPU, memory or output
size. A granted directory may contain credentials, executable code, hard links or service sockets;
that data and delegated authority are exposed. A workspace containing the application home also
contains host configuration: a writable workspace grant does not promise to protect files inside
it. Do not claim safety for credentials or UNIX sockets inside an allowed tree. With isolation
disabled, commands have the existing host user's ordinary access; disabling is an explicit local
policy choice. The [CLI guide](../../crates/dim-cli/README.md#command-isolation) documents these
limits for users.

## Configuration owner and format

HostPlatform owns `host-isolation.json` under the execution node's application home, through
its declared FileStorage dependency. It is not project, client or Session configuration; remote
execution reads the remote node's file. HostPlatformConfig supplies an optional file location,
as with exec-shell.json. The location is structural; the file's policy is business data.

The file is a new, versioned format. Version 2 accepts `sandbox`, `escalation`, `readOnlyPaths`,
`writablePaths` and `environment`. Omitted `escalation` uses `ask`;
each other omitted policy field uses the default in the table;
an explicitly empty list grants none of that resource. Only `version` is required when a file
exists. Version 1 files stay valid: they name the tier with `enabled` and `network`, and never
permit escalation. Examples:

```json
{"version": 2, "sandbox": "host"}
```

```json
{"version": 2, "sandbox": "workspace", "escalation": "ask"}
```

An explicit Linux policy can keep the default tier, allow a command to ask for the network, let
git read the user's global configuration and name a writable workspace:

```json
{
  "version": 2,
  "sandbox": "workspace",
  "escalation": "ask",
  "readOnlyPaths": ["/usr", "/bin", "/sbin", "/lib", "/etc", "/home/user/.gitconfig"],
  "writablePaths": ["/workspace/project"],
  "environment": ["PATH", "HOME", "LANG", "TERM", "NO_COLOR"]
}
```

`sandbox` is `workspace`, `network` or `host`. `escalation` is `ask` (default) or `deny`: `deny`
refuses every request above the node tier, `ask` runs a request Permission admits at the tier it names.
Version 1 maps `enabled: false` to `host`, `network: true` to `network` and everything else to
`workspace`, with `escalation` denied. A field that belongs to another version, an unknown field,
an unknown version, malformed/unreadable files, non-absolute or nonexistent roots, a read-only
root that is neither a directory nor a regular file, a writable root that is not a directory, and
invalid environment names are errors.

The path list is node-specific; it is not a portable Linux runtime promise. Each entry of
either list is a root. A read-only root is an existing absolute directory or regular file; a
writable root is an existing absolute directory. A file root grants that file alone, not its
directory or siblings, so a user's own list can keep a configuration file such as git's without
exposing the rest of the home. Sockets, FIFOs and devices are not file roots: a read grant does
not let a client connect to a socket, and read-only binds do not permit device access. Writable
roots stay directories, because a tool usually replaces a file by renaming a new sibling over it,
which a single-file grant cannot allow. Preserve executable and dotfile aliases at their requested
absolute destinations while resolving/validating their host roots once at this external boundary.
Names are nonempty and contain neither NUL nor '='. A declared or resolved read-only root cannot equal or
be inside a writable root in the same path space: neither backend carves read-only exceptions out of a
writable parent. A writable child inside a read-only root is supported.
Writable roots also permit reads; grants are additive. Naming an environment value grants no
filesystem access to the path it contains. No file is written merely because defaults are used.

Read at each model process start; no cache, watcher, Session subscription, loader reload or
restart of an existing shell. No configured location, or a missing file at that location, uses
the defined default. An existing invalid file fails explicitly. The `host` tier returns the
ordinary ProcessSpec without requiring a sandbox backend. Windows skips this unsupported policy
and retains ordinary execution until it has an accepted backend contract.

## Trusted local policy management

HostPlatform owns typed `read_isolation_policy` and `update_isolation_policy` operations over
its existing FileStorage source. The App Server exposes them only through trusted local
management methods, never paired-device RPC, Session/branch services or model tools. The CLI
uses this management path, not model `exec`; it remains available when commands are denied.
No new Settings document, workspace setting, Timeline event or policy cache is introduced.

| Management method | Request | Reply |
| --- | --- | --- |
| `host.isolation.get` | `{}` | `{revision, path, source, policy, effective}` |
| `host.isolation.update` | `{expectedRevision, policy}` | The same snapshot as `get` |
| `host.isolation.reset` | `{expectedRevision}` | The same snapshot as `get` |

`path` is the actual execution-node policy path. `source` is `defaults` for an absent file or
`file` for an explicit file. A node without a configured policy location reports management
unavailable rather than inventing a client-local file. `policy` is the editable versioned file
with omitted fields preserved (null list values are rejected); for an absent file it is the
version 2 product-default document `{version: 2, sandbox: "workspace", escalation: "ask"}`.
`effective` contains the resolved `sandbox` and `escalation`. Environment fields contain names
only; no operation reads or returns credential values. Existing version 1 files remain readable
and writable under their own schema; changing their tier/escalation in the CLI explicitly
converts their effective tier/escalation to version 2 while preserving optional lists.

`revision` is an opaque content revision that distinguishes absence from file bytes and detects
external edits as well as other clients' saves. Update/reset acquire the existing FileStorage
file lease, reread and compare the revision, validate the complete proposed document using the
same boundary rules as process preparation, then atomically replace the file under the lease.
Validation includes explicit roots and their overlap even at the `host` tier. A stale revision
returns `isolation_policy_conflict` without writing; invalid input returns invalid params;
read/validation failures in persisted data or storage failures return `isolation_policy_failed`.
A conflicting or invalid write does not alter either the policy or running processes. Storage
post-commit durability errors retain FileStorage's error contract; clients reread before retrying.

Reset atomically saves the explicit version 2 product-default document above, discarding custom
lists so they once again use resource defaults. It does not remove the file: its source remains
`file`, and future product-default changes do not silently reinterpret this saved policy. Updates
and reset apply to the next command or new persistent shell, not an existing process.

Isolation denial, unavailable-backend and sandbox failure diagnostics name both the actual
policy path and the supported `/settings > Command isolation` configuration entry. They
distinguish admission from process isolation. In particular, `network` does not grant Homebrew
executable or GitHub credential access: a Homebrew `gh` workflow must request authorized `host`
access or configure explicit roots, then provide its ordinary credential source separately.

## Existing call flow

1. WorkspaceTools builds its existing ProcessSpec. ShellSession selects its existing persistent
   shell. Their owning Session configuration supplies the workspace separately from the model's
   requested working directory. ShellInput never enters this preparation.
2. A crate-local HostPlatform operation prepares that spec once before spawn:
   `prepare_isolated_process(spec, workspace: &Path, output_dir: Option<&Path>, requested: Option<Sandbox>).await -> Result<(ProcessSpec, Option<TemporaryDirectory>, Option<Isolation>), ProcessSpecError>`.
   Only ShellSession supplies its immutable, host-owned current branch output directory.
   WorkspaceTools passes the resolved `sandbox` of the admitted `exec` call; ShellSession passes
   `None`, which keeps the node tier.
   It reads policy, resolves the tier, selects the existing environment names and constructs
   backend argv/profile.
   The second value is the process's [temporary directory](#temporary-directory), when one applies.
   The third is the crate-local `Isolation` value of a wrapped spec, which the consumer keeps with
   its process for [failure notes](#visibility). The `host` tier and unsupported Windows
   return the original spec and neither value. No public service, protocol or ProcessSpec field is
   added.
3. The consumer calls existing synchronous spawn or spawn_pty and attaches the temporary directory
   to the returned tree. Backend setup precedes payload
   execution; a failed setup runs no payload. Arguments and paths stay separate argv values.
4. Existing process/PTY trees, pumps, cancellation and terminal facts remain with their current
   owners. The background handoff moves the already-running ExecProcess; it does not start another
   namespace, change policy or require another supervisor.

Raw HostPlatform spawn/PTY remain available for ShellInput and trusted host uses. Product isolation
is promised only for the listed model consumers. Shell/runtime discovery stays outside the command
sandbox. ShellInput Q4 keeps its user commands outside F5; their normal-exit/disconnect behavior is
unchanged.

## Platform bindings

### macOS

Use the measured `/usr/bin/sandbox-exec` argv entry point with a HostPlatform-owned generated
Seatbelt profile. Start from deny-default, allow reads only for the configured roots and
required runtime devices, writes only for writable directories/devices, and deny networking when
network is disabled. Explicitly enabled network permits IP operations and the system DNS
resolver socket at `/private/var/run/mDNSResponder`, with metadata access to its `/var` alias.
Other UNIX-socket connections remain limited to the granted directory trees, with local socket
binding limited to writable trees.
Use profile parameters for host paths; never embed an unescaped path into policy source. The
profile is internal implementation data, not a second user-configurable policy language.
A directory root becomes a `subpath` rule and a file root a `literal` rule, each at its canonical
source and, when that differs, its requested destination.
Each granted root, at its canonical source and its requested destination, also permits metadata
lookups on its path ancestors, as Apple's system profile does for firmlink components. This allows
`stat` of those directories, not listing or reading them. `realpath(3)` examines every path
component: without it, Python and Clang in the developer directory cannot locate themselves, and
the same holds for executables in configured roots.
Inherited PTYs permit only terminal-mode, geometry and foreground-group ioctl commands on tty
devices. This permits terminal detection, resize and job control without granting device opens
or terminal-input injection.

The original spike's broad `file-read*` allowance is not an implementation of this contract.
The current narrow-read fixture passes on macOS 27.0.1 with a literal `/` read in addition to
its named read roots. Controlled probes fail before payload output without that literal; the
specific failing syscall has not been identified. Keep the literal directory grant, not a
recursive root read. Outside reads/writes/chmod, symlink escapes and host IPv4/IPv6 TCP/UDP
are rejected; process-tree and PTY lifetime checks pass. Actual model exec and configured
persistent-shell flows cover local policy, environment selection, terminal resize, task stop and
Session close. Strict-default persistent background execution still awaits the output decision
below. Network-enabled native socket flows verify IP traffic and the UNIX-socket path boundary
against live listeners, with explicit file grants as the positive UNIX-socket control.
Do not compensate for another baseline's failure with broader reads, networking or an
unisolated fallback. Report an incompatible platform explicitly.

The /usr/bin developer shims (`git`, `python3`, `clang`, `make`) run the tools in the effective
developer directory: `DEVELOPER_DIR` when the command's environment sets it, otherwise the
directory that `/var/select/developer_dir` selects, read through the /var alias. The default grant
follows the same rule at each policy load, using the environment the command receives, and
DEVELOPER_DIR is in the default environment list, so the shims inside the sandbox use the selection
that the grant covers. The link is resolved by the ordinary root resolution, so the grant follows
`xcode-select` rather than a fixed install location. With the Command Line Tools selected,
macOS 27.0.1 needs neither `/private/var/db/xcode_select_link` nor `/Library/Preferences`. Without
TMPDIR, the shims fall back to the ungranted /tmp for their lookup cache and print errors on every
call. An Xcode.app selection resolves to its `Contents/Developer` directory, which is not enough:
on a host with Xcode selected, the shim stats the bundle's `Contents/Info.plist` and then runs
`xcodebuild`, which loads frameworks from the bundle outside `Contents/Developer`. Its lookup cache
lives in TMPDIR, which starts empty for every process. An Xcode.app selection is therefore not part
of the supported default baseline. Supporting it needs a decision on granting the bundle and
whatever `xcodebuild` requires, which has not been measured. HostPlatform never substitutes the
Command Line Tools for the user's selection, because that would silently change the toolchain.
Such a host has three local choices: select the Command Line Tools with
`xcode-select -s /Library/Developer/CommandLineTools`, start dimcode with
`DEVELOPER_DIR=/Library/Developer/CommandLineTools` in its environment, or disable isolation.

Git exits when its global configuration exists but cannot be read: it treats only a missing file
or EACCES as absent, Seatbelt reports EPERM, and even `git --version` reads the file. With the home
ungranted, git would be unusable for most users. The default therefore grants read access to the
files git reads for its global configuration, each only where it exists:

- `$HOME/.gitconfig`;
- `config`, `ignore` and `attributes` in the XDG git directory: `$XDG_CONFIG_HOME/git` when the
  command's environment sets XDG_CONFIG_HOME, otherwise `$HOME/.config/git`.

Each is a single-file grant (a Seatbelt `literal`) plus its canonical path when that differs, as
when a dotfile manager makes the file a symlink. The XDG directory is granted file by file, not as
a subtree, because git's credential store also reads `credentials` there. HOME and
XDG_CONFIG_HOME come from the environment the command receives, the same selection that passes
HOME through, so the grant covers the files git reads. Ancestor metadata follows the root rule
above.

Identity, aliases and excludes are tool configuration the user expects to apply. Exec read
isolation is not the model's secret boundary: the `read` tool is path-independent and runs without
approval under workspace-write ([session files §11.2](appserver/session-files.md#112-requirement-for-path-scoped-file-access)),
and the network is off by default, so these named files add negligible exposure.
`~/.git-credentials`, the XDG `credentials` file, `~/.ssh` and every other home path stay
ungranted. This is a narrow tool-configuration grant, not the broader reads that the paragraph
above forbids as compensation for a failing baseline.

The grant is internal to the default baseline: an explicit `readOnlyPaths` replaces every
default, git's configuration included. `include.path` and `includeIf` targets elsewhere in the
home are not granted, so git then fails with `Operation not permitted`; a `core.excludesFile` or
`core.attributesFile` elsewhere only draws a warning and is not applied. An explicit list keeps
these files by naming them as file roots, beside the default roots it repeats. A file root gets
the same `literal` grant, canonical path and ancestor metadata as the default files. Listing a
directory that contains them instead also grants everything else in it, such as the XDG
`credentials` file.

The local `sandbox-exec(1)` manual marks the interface deprecated. Support is limited to actual
OS versions that pass the isolation acceptance flows; macOS 27.0.1 fixture results are
evidence for their exercised profiles only.


### Linux

Use bubblewrap with an empty mount namespace and configured read-only/writable binds at their
original destinations, private PID/IPC namespaces, a private /proc and minimal /dev. Retain the
unprivileged user, use a private user namespace and drop payload capabilities. Resolve installed
bwrap through the captured host PATH; the host must trust that PATH and the selected executable,
including its writable ancestors. No executable provenance or integrity check is promised.
Do not download or elevate a backend. With network disabled,
unshare the network namespace; with network enabled, retain the host network. Backend failure
while enabled refuses execution, rather than selecting a weaker or unisolated path.

Each root is bound from its canonical source at its requested destination; a file root is a
read-only file bind. bubblewrap applies binds in argument order, and a bind inside an earlier one
covers that part of it. It creates a missing destination, and any missing parent directories with
mode 0755, in the new root. Read-only directories therefore come first, parents before children;
then read-only files, which may lie inside them but never inside a writable root; then writable
directories, parents before children, which may lie inside read-only directories. A file root
outside every directory root appears in otherwise empty parent directories that bubblewrap
creates, and its siblings stay absent.

The `network` tier does not grant resolver files or service sockets outside the configured roots.
The default mounts exclude `/run`; Linux resolver setups whose `/etc/resolv.conf` points into
`/run/systemd/resolve`, or whose NSS module requires a socket there, are not supported by the
default policy. The exercised Debian baseline uses a plain resolver file; systemd-resolved
hostname resolution has not been verified. No resolver mount or broader default read grant is added.

The default mounts exclude the home, so git finds no global configuration on Linux and runs
without it, where macOS grants it. Widening the Linux default is a separate decision; an explicit
list can name git's files as file roots. Their bind order is checked without bubblewrap, and
their runtime flow under bubblewrap awaits a run on a Linux machine, since CI has no Linux job.

Retain the existing ProcessTree/PtyProcessTree and bubblewrap parent-death handling. Native spawn
already establishes a session and piped output; PTY spawn owns its terminal. Do not add
`--new-session` to the PTY path: measured input/resize still worked but controlling-terminal/job
control did not. No terminal proxy, custom seccomp system or process supervisor is added.
See the upstream [policy limits](https://github.com/containers/bubblewrap/blob/main/README.md) and
[options](https://github.com/containers/bubblewrap/blob/main/bwrap.xml). The existing
[native-handle calibration](host-platform-isolation-spike.md#linux-bubblewrap-process-and-terminal-calibration)
checks tree settlement, PTY/job control and live IPv4/IPv6 TCP/UDP and UNIX-socket boundaries on
Debian 13.7 / Linux 7.2.9 ARM64, UID 501, bubblewrap 0.12.0. It does not verify product policy.

Landlock is not selected for this boundary: the measured ABI 4 wrapper does not cover UDP/UNIX
sockets or all read-only metadata operations. See its [kernel contract](https://docs.kernel.org/userspace-api/landlock.html).
Those measured limits are not claims about every newer ABI.

### Background lifetime

The existing [BackgroundExec](../architecture/rust-agent/plugins/WorkspaceTools.md#backgroundexec) owns model
background tasks. `exec::hand_off` passes the same live ExecProcess to `BackgroundExec::start`;
its existing task retains the tree until completion. Returning a ToolResult or stopping a turn
therefore does not drop that process. There is no cross-command namespace PID handoff.

| Transition | Existing owner contract |
| --- | --- |
| Foreground becomes background | Move the live process/tree and its startup policy; retain the same namespace. |
| Turn stops | BackgroundExec keeps the task running and retains its completion/wake ownership. |
| Explicit task stop or branch/Session unload | Existing task cancellation terminates/releases the owned tree before terminal settlement. |
| Command completes normally | Record exec/ended once and publish the existing bounded wake; no orphan process is reclassified as a new task. |
| App Server dies | Backend parent-death handling terminates isolated descendants; no work is recovered from history. |

Shell backgrounding inside a command does not create a BackgroundExec task: normal root exit
completes that command, with the backend's namespace cleanup. Long-lived model work uses the
existing runInBackground handoff and keeps its command root alive.
Task control uses the existing taskId and host-owned tree, not `kill $!` from a fresh namespace.
ShellSession likewise retains its original PTY/tree for the shell's lifetime. Product acceptance
must verify transfer, later task stop and Session shutdown with the actual isolated model paths.

ShellSession reads a background job PID once at its existing PTY marker boundary. HostPlatform
uses the owned PTY's existing foreground-group handle to identify the shell's Linux PID namespace
and OS session, then resolves that runtime identity through the kernel's `/proc` fields. The
private task stores a typed host PID or its actual I/O error; stop and shell-exit consumers do not
parse strings or resolve identities again. The existing taskId and persisted records are unchanged.
The native stop operation waits for the group to release, escalating after the existing bounded
grace, before ShellSession records a cancelled terminal fact. The public synchronous signal helper
retains its signature. No PID mapping table, shared lock, namespace registry or supervisor is held.

The separate [normal-exit calibration](host-platform-isolation-spike.md#linux-bubblewrap-normal-exit-and-background-compatibility)
shows redirected `!cmd &` descendants do not satisfy ShellInput's outliving/PID contract under the
candidate private-PID/parent-death arguments. That is valid evidence for excluding user ShellInput
from isolation; it is not proof that a held BackgroundExec tree dies when its tool call returns.
There is no cross-command namespace/PID handoff, detached-namespace registry or new stop protocol.

### Windows

Windows has no supported isolation backend. It keeps the original exec/PTY, environment and tree
ownership and does not load this file. Do not present that path as isolated or simulate a sandbox
with path checks or Permission rules.

## Failure and persistence

Policy parsing/setup failure runs no payload. Enabled isolation has no implicit fallback; the
`host` tier is ordinary execution. A request above the node tier without `escalation: "ask"`
returns `isolation_denied` with its fix, naming the policy file, and runs nothing. Approval widens
neither the filesystem nor the network: it admits a call whose `sandbox` the node policy already
allows, and the authority stays the node's file (ADR 003 D5). Existing consumers report their
existing typed error/result; no new approval category or Timeline fact.
A missing Linux backend returns `isolation_unavailable` with its fix: install bubblewrap, or set
`"sandbox": "host"` in the policy file, which the text names.
The policy is a startup snapshot. Concurrent path/policy edits add no atomicity guarantee, grant
lease or filesystem virtualization. Restore/fork uses current local policy for each new process.

## Visibility

A denial reaches the command as an ordinary error, such as `Operation not permitted`, so HostPlatform
states the boundary where the model plans commands and where a command fails.

**Tool declarations.** On macOS and Linux, the `exec` and persistent-shell `execute` declarations
carry one static line from HostPlatform: unless the user disabled isolation, commands are
sandboxed, write only inside the Session workspace (on macOS also `$TMPDIR`, the
[temporary directory](#temporary-directory)), read only there and in system directories, and have
no network; `exec` accepts a `sandbox` argument to request a wider boundary, the node's
host-isolation.json decides whether that request is allowed, and approval does not widen anything
by itself. The line describes the default policy and the argument, not the current file. ToolRegistry
reads declarations synchronously when it applies or attaches a generation
([C3](contracts/session-boundary.md#a5-tool-contribution)), while each process start reads the
policy (ADR 002 D4). A declaration built from the file would be a second snapshot that a later
edit makes wrong. Windows declarations carry no line.

**Failure note.** `Isolation` records where the user changes the policy: the configured file's
path, or the file name when no location is configured. When an isolated `exec` command exits
unsuccessfully, in the foreground or after the background handoff, and the output shown in its
result contains a typical denial, the result text ends with a note. It says that isolation may have
caused the failure, what the default blocks (as the declaration line describes it), that approving
or retrying the command does not lift it, and that the user can change the policy at that location.
Typical denials are `Operation not permitted` (EPERM) and dyld's `blocked by sandbox` on macOS,
`Read-only file system` and `bwrap:` diagnostics on Linux, resolver failures and
`Network is unreachable`. When the developer tools lack a grant, as with an Xcode.app selection
or an explicit `readOnlyPaths` without the developer directory, the macOS shims' `xcode-select`
and `couldn't stat Xcode's Info.plist` errors carry the EPERM text, as does git's
`unable to access` error for an ungranted configuration file. The note is a heuristic. The
backends report no denial event, ungranted Linux paths are simply absent
(`No such file or directory`), and a matching message can have another cause. The note is text in
the existing result content;
structured content, codes, Timeline facts and approval categories are unchanged, and it offers no
approval or rule. Successful commands, timeouts, disabled policy and Windows get no note. The
persistent shell's `execute` results carry none: that tool is not in the App Server's product
assembly, and ShellSession can keep the same value if it joins.

**CLI status.** `/settings > Command isolation` shows the effective tier, wider-request policy,
source and actual path through the local management methods
([Trusted local policy management](#trusted-local-policy-management)); it remains usable when
model commands are denied. There is no indicator beside the permission preset: isolation is the
execution node's local policy, read at each process start, so a value shown in the panel can be
stale by the next command if the file changes elsewhere, and another node's policy is not
reported. The CLI guide documents the policy and states that presets and approvals do not change
it, and the failure note names both the file and the settings entry.

**User documentation.** The [CLI guide](../../crates/dim-cli/README.md#command-isolation) has the user page: the
file location for each profile, the fields, how to disable isolation from `/settings` or by
editing the file, opt-in toolchain roots and
what isolation does not cover. For macOS it also describes the temporary directory, developer
tools and git configuration in user terms, what an explicit list drops, and how a list grants
git's configuration files as file roots. Per-platform defaults stay in this design's
table and the [macOS](#macos) section.

## Persistent shell output

Persistent ShellSession adds only its host-owned current branch output directory,
`ShellSessionConfig.output_dir` (normally `HostPlatform.home/.dim/exec/<session>/<branch>`),
to the default writable roots alongside the Session workspace. The shell owner creates this
directory before preparation; the model's cwd and command cannot select or expand the grant.
An explicit `writablePaths` list fully replaces both defaults. In particular, `[]` grants no
directory writes, and a workspace-only list does not authorize the shell output directory.

Existing output paths have different owners. WorkspaceTools' `ExecProcess` pumps child output
into its host-owned output sink; an occupying ShellSession command appends its captured PTY
text from `OccupyingTask::commit`. Explicit POSIX background jobs instead open their log inside
`posix_background`; the shared PTY carries the PID/completion markers, and the host reads the
log for previews. The current PTY capture has no per-background-job output framing. Reusing it
for concurrent jobs would require changing ShellSession's output transport and attribution;
the existing host pump alone does not remove the background wrapper's file open.

Existing stdio/PTY remain runtime channels rather than directory grants. Persistent background
redirection fails when local policy omits write access to its output directory; the payload must
not run after that failure. Ordinary WorkspaceTools receives no output-directory grant because
its host captures output. No Core API, protocol, ProcessSpec field, lifecycle phase or supervisor
is added. The startup snapshot applies to these default roots as to explicit policy. A sibling
branch's output directory and the rest of the home, apart from git's configuration on macOS,
receive no implicit access. Unreadable login
startup files outside the grants can produce shell diagnostics; preparation does not silently
read the user's home or discard that output.

## Temporary directory

On macOS, when isolation is enabled and `writablePaths` is omitted, preparation creates one empty
directory for the process tree, adds it to the default writable roots and sets TMPDIR to it. It is
created with mode 0700 and a fresh `dim-isolated-<uuid>` name under the App Server's system
temporary directory (normally the per-user `/var/folders/…/T`). The parent directory, its other
entries and /tmp stay ungranted. Each WorkspaceTools exec process receives its own directory;
a persistent ShellSession shell receives one for its lifetime.

The process tree handle owns the directory. The consumer attaches it to the ProcessTree or
PtyProcessTree it spawns, and dropping that handle removes the directory on a blocking worker.
A background handoff moves the handle, so BackgroundExec keeps the directory until it settles the
task. A failed spawn removes it at once. Descendants that outlive their reaped command root lose
the directory. If the App Server dies, the directory stays in the system temporary area for the
OS's ordinary cleanup; there is no startup sweep, registry or recovery.

The directory is a default writable root, like the Session workspace and the shell output
directory: an explicit `writablePaths` list replaces it. Then no directory is created, TMPDIR is not
set, and a TMPDIR named in `environment` passes through unchanged and needs its own grant. The
`host` tier and Windows keep ordinary execution. Failure to create the directory refuses the payload
with `isolation_unavailable`.

Linux is unchanged in this revision. Its namespace has no /tmp and receives no TMPDIR; binding the
same directory there needs a Linux acceptance run first.

## Impact and acceptance

| Location | Necessary change |
| --- | --- |
| HostPlatform | Local policy parsing/defaults, tier resolution and backend transformation, including directory and file roots, the macOS developer-tool roots, git's global configuration, ancestor metadata and the per-process temporary directory owned by the tree handle; reuse FileStorage, ProcessSpec and error types. |
| WorkspaceTools / ShellSession | Pass immutable Session workspace and the `exec` call's resolved `sandbox` (ShellSession passes none) to preparation before existing spawn/PTY and attach the returned temporary directory to the spawned tree; ShellSession also supplies its owned output directory for default writes. Preserve admission and tree ownership. `exec` declares the argument and keeps the returned `Isolation` with its process for the failure note. |
| BackgroundExec | Preserve its existing live-process handoff, taskId control and terminal facts; no new process service. |
| ShellInput | None; user commands continue ordinary spawn. |
| Product assembly | Supply optional local file location through existing HostPlatformConfig/conditional FileStorage; default policy works without a file. |
| Owner docs and flows | Supported baselines, real model exec/background/PTY acceptance and explicit disabled/Windows paths. |

Verify actual model foreground/background exec and persistent shells on macOS/Linux: default
system-shell plus workspace read/write without a file (on macOS also git with the user's
identity, aliases and XDG excludes from a plain or symlinked global configuration, python3 with
DEVELOPER_DIR naming the Command Line Tools, and a TMPDIR file, with /tmp writes, other home files
and a home listing still denied), configured stricter/wider roots, file roots that let git read
a plain or symlinked global configuration while other home files stay denied, symlink
escape, outside file/metadata denial, TCP/UDP denial with network disabled and success against live
listeners when enabled, ungranted UNIX sockets, PTY input/resize/job control, tree stop/unload and
background task survival across a turn stop. Verify later taskId stop, one terminal/wake, Session
shutdown and no process replay. Policy edits affect only new processes; malformed existing files
refuse payload startup; the `host` tier executes normally without requiring a backend.
A model-supplied cwd outside the workspace must not enlarge default writes. Reuse ShellInput's
existing ordinary `!`/`!!` flows as the exclusion control. Windows uses ordinary exec/PTY.
Isolated foreground and background exec failures with a typical denial end with the note naming
the policy file; success, other failures and host-tier execution get none. Linux without bubblewrap
reports the install-or-host fix. An `exec` call whose `sandbox` is above the node tier fails with
`isolation_denied` before any payload, names the policy file, and leaves no file behind; the same
call runs outside the workspace once the node sets `escalation: "ask"`, while a narrower request
applies under any policy.

Use disposable homes, roots and local listeners. Mechanism probes do not establish defaults,
product policy loading or the real consumers. F5 closed after [#189](https://github.com/arcships/dim/pull/189) delivered the isolated model
exec and PTY flows with tests on macOS and Linux. PR CI runs them on macOS only; it has no Linux
job. CLI startup timing is a separate fix, [#158](https://github.com/arcships/dim/pull/158).

## Rule check

| Rule | Application |
| --- | --- |
| §1 | Extend the existing process owner; no new capability, registry, supervisor or lock. |
| §4 rule 1 | Consumer admission/results retain their existing owners. |
| §4 rule 2 | Background transfer retains the live tree and its startup boundary. |
| §4 rule 3 | No before callback or commit-time I/O. |
| §4 rule 4 | Core and assembly contain no isolation business branch. |
| §4 rule 5 | No new batch or dispatch semantics. |
| §4 rule 6 | Only existing tool results and adopted inputs enter model context. |
| §4 rule 7 | Public HostPlatform/tree interfaces and declared FileStorage only. |
| §4 rule 8 | Existing mechanisms suffice; platform limits stay in F5. |
| §4 rule 9 | Host policy and diagnostics are not Timeline facts. |
| ADR 002 D1 | Host owns local policy; consumers own their executing process lifetime. |
| ADR 002 D2 | No wiring or cross-scope policy service. |
| ADR 002 D3 | No optional isolation-provider dependency. |
| ADR 002 D4 | Read business policy per process; loader retains only its structural location. |
| ADR 002 D5 | Existing data/error/process contracts; no new language-neutral service. |
| ADR 003 D1 | The `sandbox` request is part of the model's tool call and is admitted like any other. |
| ADR 003 D5 | A preset and an approval confer no boundary; the node's file alone decides a wider tier. |

## Requirements raised to mechanism owners

None. Defaults, configuration and backend acceptance belong to HostPlatform. No new readiness,
subscription, protocol, supervisor or rutis feature is required.

## Ablation

One local file and its explicit defaults replace a policy service or watcher. One preparation
operation reuses existing spawn/PTY and native errors. BackgroundExec already holds the process;
no namespace/PID manager is needed. User ShellInput stays outside this boundary. The tiers are the
existing two settings named as one value, and a wider request is one extra call input resolved
against the same file, not an approval category, per-command matrix or new result field. The
policy is decoded once at its local-file boundary; configured roots retain their canonical source
and requested absolute destination.
Explicit policy fully replaces default roots; no separate runtime-directory exception is needed.
The developer directory comes from DEVELOPER_DIR or the existing link resolution, not a probe, a
fixed install path or a substituted selection. Git's configuration is a few named files in the
default baseline, not a home read or a subtree. A file root reuses root resolution and the
backend's file grant (a Seatbelt `literal`, a bubblewrap file bind).
The temporary directory lives and dies with the tree handle that already owns the process; no
sweeper, registry or policy field is added.
A static declaration line and a text note in existing results replace a declaration-time policy
read, a new result field, an approval or a client status read.
