# CLI commands and exports

F6 (#77) covers the command entry points, conversation export and JSONL
execution listed in [overview Appendix D](overview.md#appendix-d-cli-layer) and
the [feature list](../feature-list.md#cli-commands). The native CLI remains a
client of the existing daemon. This document defines the CLI interface for
the implementation; the [native CLI contract](../../crates/dim-cli/README.md)
describes the current executable.

The [attachment input contract](cli-attachment-input.md) defines the idle draft editor,
local image sources and paste handling. The multiline/paste editor and headless exec image
arguments, clipboard/file insertion, completion and dropped paths are implemented. The attachment
contract records the native platform acceptance evidence and outstanding checks.

## Commands

| Entry | Behavior |
| --- | --- |
| `dimcode tui` | Open the existing interactive scrollback client. |
| `dimcode exec [-p TEXT] [--image PATH ...] [--json]` | Run text and/or local images and exit; read redirected stdin when `-p` is absent. |
| `dimcode exec resume SESSION_ID [-p TEXT] [--image PATH ...] [--json]` | Continue the named persisted Session with text and/or images. |
| `dimcode session export SESSION_ID --format markdown\|json [--output PATH]` | Export the main branch; write to stdout when no output path is supplied. |
| `/export markdown PATH` or `/export json PATH` | Export the current main branch to a file while the interactive client is idle. |
| `/exit` | Detach and exit from the idle interactive prompt. |
| `!command` / `!!command` | Call the Session's existing `shell.run` in record/private mode, including during a model turn. |
| `--permission read-only\|workspace-write\|full-access` | Select the preset of a new or resumed Session for `dimcode`, `tui`, `exec` and `exec resume` ([permission controls](#permission-controls)). |
| `/approvals` (alias `/permissions`) | Show, switch and manage the preset and command rules while the interactive client is idle. |

Global home, workspace, server and model-profile options retain their current
meanings and work before or after the command. Existing login/account/logout,
models, MCP and daemon commands remain available. Existing `--prompt`, piped
stdin and `--resume` calls retain their behavior; `exec resume` also accepts the
same existing global options. Supplying the Session twice or the prompt twice
is an argument error. `--json` is an `exec` option, and `tui` requires a terminal.
Help/version and argument errors do not start the daemon.

The interactive export parser reads the format token and treats the remainder
of the line as a literal path, including spaces, without shell evaluation or
quote expansion. Missing paths and unsupported formats print usage and return
to the prompt without submitting input. `/exit` takes no arguments. Only these
exact command names are intercepted; other text follows the normal input path.
Subcommands do not create a plugin command registry.

P1 shell prefixes apply only to interactive input. Their execution, recording,
progress, cancellation and lifetime follow [ShellInput](plugins/ShellInput.md).
The CLI generates `runId` before sending the request and never retries a run.
The command after the prefix is passed verbatim; an empty command prints usage.
While a command is pending, a second shell line reports that the caller already
has a run. Another client's run can still cause the server's `shell_busy` error.
Neither case submits model input. Ordinary text typed during execution retains
the native client's discard behavior, with a message to wait for the next prompt;
there is no input queue. A pending approval owns its answer line, including `!`.

The first Ctrl+C during a shell run sends `shell.stop` for its client-generated
identity, including before admission. A second interrupt uses the existing model
turn stop and waits for settlement; it never skips pending replies. EOF stops
the caller's shell run and model turn before detaching. With no run, the existing
interrupt behavior applies. Progress is labelled as incomplete; the final reply
prints the authoritative bounded output again, its status, truncation, and the
recorded sequence or recording error. Output is filtered at the terminal boundary.
RPC errors remain command errors, never model text. A disconnected caller does
not imply that a run stopped or that its recorded input was lost.

```text
> Explain the failing build
Assistant: Inspecting the build...
!cargo check
[shell started] record
[shell progress] (incomplete)
    Checking project
^C
[shell stopping] Waiting for command settlement
^C
[stopping] Waiting for resource settlement
[shell output] (final)
    Checking project
[shell stopped] ...
[shell recorded] sequence: 42

> !!printf private
[shell started] private
[shell output] (final)
private
[shell completed] exit: 0

> _
```

These records use terminal scrollback and wrap at the terminal's width; there
is no second prompt, overlay or client-side process. Best-effort progress may
be dropped. Final output is shown in full rather than inferred from progress.

For both export entries, relative output paths use the CLI process's current
directory, including after resume; neither the Session's cwd nor `--home` changes
that base. The CLI treats `~` literally. Usage/help states: `PATH is relative to
the CLI current directory; ~ is literal; use an absolute path for another base`.
An invoking shell may expand an argument before the CLI receives it.

Other target commands require their owning features. F6 does not implement a
fullscreen interface, persistent RPC/ACP server, verification/repair loop,
provider editor, Session deletion/fork, or model-request snapshot export.
`dimcode export` remains reserved for the distinct request snapshot feature.

## Terminal interaction

```text
$ dimcode tui --home /tmp/dim-work
dimcode · model: provider/model
workspace: /workspace/project
session: <session-id>

> /export markdown
Usage: /export <markdown|json> PATH
PATH is relative to the CLI current directory; ~ is literal.

> /export markdown /tmp/Session notes.md
Exported Markdown: /tmp/Session notes.md

> /export json /tmp/events.json
Exported JSON: /tmp/events.json

> /exit
$

$ dimcode session export <session-id> --format markdown > notes.md
$ dimcode exec -p 'Explain this project' --json > run.jsonl
```

The prompt keeps keyboard focus and the terminal keeps scrollback. There are no
new dialogs, overlays or fixed-width columns. Paths and error messages use the
existing terminal text filter; narrow terminals wrap normally. An empty Session
exports an empty event list or a Markdown heading without fabricated messages.

## Execution and JSONL output

`exec` uses the existing create/attach/history/submit/idle/detach sequence and
stop settlement. A resumed running turn settles before the new prompt is
submitted. As the attached interaction client, `exec` actively sends the ordinary
denial response to an approval request, stops the turn, waits for stop
settlement, and exits with code 3. It does not change
`SessionConfig.interactive`. Sessions created as noninteractive retain Approval's existing owner-side policy (#98 D10).
`--permission` changes only which calls need approval; a call that still asks is denied
the same way ([headless execution](#headless-execution)).
Other exit codes retain the native CLI contract, including 2 for turn failure
and 130 for interruption. A transport failure does not retry a submission.
Repeated `--image` arguments follow the headless attachment contract: text first,
then validated original image bytes in argument order, with paths relative to the
invoking process. An image-only invocation is valid. A submission error on a live
connection keeps the uncertain-outcome diagnostic/exit path unless its `error.data.appError`
is one of the six [guaranteed pre-commit rejections](../../crates/dim-app-server/README.md#session-submission-errors):
`invalid_params`, `session_invalid_input`, `session_closed`, `session_build_failed`,
`service_reloading` or `unknown_method`. Those retain their ordinary error diagnostics.
Every other error, including `submission_unknown` and opaque publication/storage failures,
can follow commit and must not trigger another send.

In JSON mode, stdout contains one compact JSON-RPC notification per line using
the daemon's existing `method` and `params`. Only notifications for the selected
Session are emitted. Protocol handshake, RPC replies and other Sessions never
enter this stream. Session handles and human diagnostics go to stderr; normal
assistant/tool rendering does not also print to stdout.

```json
{"jsonrpc":"2.0","method":"session.model.progress","params":{"sessionId":"s","branchId":"main","requestId":"r","textOffset":0,"text":"Hello"}}
```

Notification fields remain defined by the existing public v3 protocol.
Live observations begin after attach; stored
history is used for reconciliation and is not dumped as execution output.
Durable events retain their sequence and public payload, including fields the
text display does not render. Model progress retains its existing method and
offsets: a progress notification is not an additional durable text fact.
JSON encoding escapes control characters; the terminal text filter does not
rewrite data in machine output. The CLI adds no synthetic lifecycle events or
second event schema. Notification delivery keeps the daemon's existing
best-effort semantics; consumers needing a fixed complete record use export.

## Conversation export

Both entries call the existing `session.history` RPC. The first response fixes
`through`; subsequent pages use that watermark and the server's `nextAfter`,
including gaps caused by filtered records. The CLI reuses its existing cursor
checks. Export does not submit input, change model context, close the Session,
or implement a server-side exporter.

The exported branch is always `main`; child branch histories are not fetched or
included. Markdown renders each `branch/create` fact with its child id as
`Child branch <id> (contents not exported).` JSON preserves that main-branch
event unchanged. `branch/create` records a request before the child is built.
For `branch/create-failed`, Markdown uses the event's `branch` field to render
`Child branch <id> was not created.` for both failed and cancelled creation.
JSON retains the failure fact unchanged. No child history is fetched to determine
the outcome. A reader can distinguish failed creation from omitted child content.

JSON output contains `{sessionId, branchId, through, events}`. Each event is the
public history event without dropping unknown fields or event kinds. This is
a conversation export, not a restore/import file or native database format.
Markdown renders user/assistant content in history order and clearly labels
reasoning, tool calls/results and non-text attachments. It does not execute
content, resolve attachment URLs, or replay tools. It preserves code fences
without allowing a message to close the surrounding tool-result fence.

The CLI reads the fixed prefix successfully before opening an output file.
Files use create-new semantics: existing paths fail without replacement.
On Unix they are created with mode 0600 through the standard OpenOptions
extension; Windows uses the native inherited ACL. Existing files are never
opened to change their permissions.
No output file is created after a history error. A write failure is reported
and may leave a partial newly created file; no success message is printed.
Export-to-stdout failures return a nonzero code. Interactive export errors
return to the input prompt and never become model input. During an admitted
interactive export, Ctrl+C reports that the CLI is waiting for the result and
continues consuming further interrupts until the operation settles. Those
interrupts do not become an exit or draft-clear action at the next prompt.

## Permission controls

The App Server already owns everything needed to decide tool calls: a branch preset fixed
at `session.create {permissionPolicy}` and changed with `session.permission.get/update`
(versioned per branch), and the user's command rules read and replaced with
`permission.rules`/`permission.set_rules` ([F2](appserver/permission-command-rules.md)). The
CLI only answered each approval once with `y`. This section adds CLI entry points over those
contracts and the [F2 §5.4](appserver/permission-command-rules.md#54-always-allow-client-flow)
client flow. The App Server, Permission, Approval, events, protocol and storage do not change.

| Preset | What runs (Permission's existing decision, after `disallowedTools` and deny rules) |
| --- | --- |
| `read-only` | Reads and Session tools run; file writes, commands, network and unclassified tools are denied |
| `workspace-write` | Reads, file writes and Session tools run; commands, network and unclassified tools ask |
| `full-access` | Every tool call runs without asking |

A deny rule applies under every preset. An allow rule only turns a question into a run for a
simple command; it never lifts a denial. The product and configured daemon profiles create
Sessions with `workspace-write`; that default is unchanged.

### Startup selection

`--permission read-only|workspace-write|full-access` applies to `dimcode`, `tui`, `exec` and
`exec resume`, before or after the command. Another value, a repeated flag or the flag on any
other command is an argument error before the daemon starts.

```text
$ dimcode exec --permission full-access -p 'Run the tests'
  hello
  session.create {cwd, permissionPolicy: {preset: "full-access"}}
  attach -> history -> submit -> idle -> detach

$ dimcode exec resume <session-id> --permission read-only -p 'Summarize'
  hello
  session.permission.get {sessionId, branchId: "main"}  -> {version: v, policy}
     preset already read-only: nothing is written
  session.permission.update {..., expectedVersion: v, policy: policy with preset read-only}
     permission_conflict: read again and retry, at most 3 attempts, then exit 1
  attach -> history -> submit -> idle -> detach
```

A new Session receives the whole policy `{preset}` in `session.create`: no tool lists, which is
the product profile's form. Agent members copy this creation policy (see branch semantics). On
resume the flag replaces only the preset; the stored `allowedTools`/`disallowedTools` are kept.
The update happens before attach, so no turn submitted by this invocation runs under the old
preset. Without the flag, `session.create` omits the policy and a resumed Session keeps its own.
A resume flag against a daemon that does not advertise `session.permission.update` fails before
attach. A conflict retry is correct here because the requested preset does not depend on what
the read returned; the read only supplies the version and the tool lists to keep.

The preset is shown where the Session is identified. Interactive start reads it with
`session.permission.get` when the daemon advertises that method; `exec` prints it to stderr only
when `--permission` was given, so its existing diagnostics are otherwise unchanged. A
`permission/changed` event that arrives live on `main`, from this or another client such as a
paired device, prints one record. The approval reason already names the preset in force.

```text
$ dimcode --permission read-only
dimcode · model: provider/model
local files: /workspace/project
session: <session-id>
permission: read-only

> Fix the build
Assistant: ...
[permission] main branch preset: workspace-write     <- changed by another client
```

There is no status line: the scrollback client has no persistent area, and the editor's hint
line is fixed help text truncated to the terminal width.

### Headless execution

`exec` keeps its contract: an approval request is answered with the ordinary denial, the turn is
stopped and settled, and the exit code is 3. `--permission` only changes which calls ask. With
`full-access` nothing asks; with `read-only` writes and commands return Permission's denial to
the model without a question; with `workspace-write` a command still asks and exits 3. There is
no automatic approval. The flag is the explicit choice, so `exec` asks for no confirmation.

### Branch semantics

Permission's state for a branch is `SessionConfig.permissionPolicy` folded with the
`permission/changed` events of its ancestor prefix and then its own. The CLI writes only `main`,
the branch it submits to, idles on and exports.

```text
session.create {permissionPolicy}  -->  SessionConfig.permissionPolicy
        |                                         |
        v                                         v  copied when a member is created
 main: permission/changed (CAS on version)    agent member Sessions (Collaboration)
   ^  written by --permission on resume
   |  and /approvals
   +-- branch point --> child branch (branch tool): main's policy at the branch point,
                         then its own changes
```

- Child branches that the branch tool creates after a change inherit it. Existing child branches
  keep their policy; the CLI does not enumerate or update them. Their approval requests are
  still answered on their own branch id, as today.
- Agent members copy the leader Session's creation policy. `--permission` on a new Session
  reaches them; a later change on `main` does not. The confirmation of a switch says so, and
  requirement R2 below asks Collaboration to copy the leader branch's current policy.
- A fork copies the source's facts, so it starts with the source's current `main` policy (#26).

### `/approvals`

`/approvals` and its alias `/permissions` run at the idle prompt like the other commands and need
plain single-line input. Without arguments they read the preset of `main` and, when the daemon
advertises `permission.rules`, the command rules, numbered. The printed rules and their revision
are kept for `remove`.

```text
> /approvals
Preset (main branch): workspace-write
  read-only        reads run; writes, commands, network and unclassified tools are denied
* workspace-write  reads and file writes run; commands, network and unclassified tools ask
  full-access      every tool call runs without asking
Command rules (user scope, every Session):
  1  allow  exact   cargo test -p dim-cli
  2  deny   prefix  git push
Usage: /approvals [PRESET | allow|deny exact|prefix COMMAND | remove N]

> /approvals full-access
Full access runs every tool call, including commands, without asking.
Switch the main branch to full-access? [y/N]
Preset unchanged: workspace-write

> /approvals read-only
Preset (main branch): read-only. Child branches created later inherit it; agent members keep the Session's starting preset.

> /approvals deny prefix git push
Rule already present: deny prefix `git push`

> /approvals remove 1
Rule removed: allow exact `cargo test -p dim-cli`
Command rules (user scope, every Session):
  1  deny   prefix  git push
```

| Form | Calls | Conflict |
| --- | --- | --- |
| `/approvals` | `session.permission.get` on `main`; `permission.rules` | none |
| `/approvals PRESET` | `get`; for `full-access` a `[y/N]` confirmation where Enter, any other answer, EOF and Ctrl+C mean No; then `update` with the read version and tool lists | `permission_conflict`: read again, print the current preset, write nothing; the user decides again |
| `/approvals allow\|deny exact\|prefix COMMAND` | `permission.rules`; unless the same rule exists, `set_rules(revision, rules + rule)`, then print the new list | read again and retry, at most 3 attempts: adding does not depend on the other rules |
| `/approvals remove N` | `set_rules(listing revision, listing without rule N)`, then print the new list | `permission_rules_conflict`: read again, print the current list, remove nothing |

`remove N` refers to the numbered list printed last. The listing, an added rule, a removal and a
removal conflict each print the resulting list, and only a printed list is kept for `remove`, so
a number never refers to a position the user has not seen. A form that prints no list (a rule
already present, a preset switch, an "always" answer during a turn) keeps the last printed one; if
the rules changed since, its revision makes the next `remove` fail, and the conflict prints the
current list. Without a printed list in this process, `remove` asks for `/approvals` first.
COMMAND is the rest of the line; the CLI rejects an empty command or one containing a separator
character (`dim_agent_types::COMMAND_SEPARATORS`, the set F2 validates) before any call. A
preset that is already in force writes nothing. Without the advertised methods, the command
reports that this daemon does not provide them. While a call is outstanding, Ctrl+C reports
that the CLI is waiting for the result, as for `/export`. Errors are printed as command errors
and never become model input.

### "Always allow"

The approval prompt offers `a` when all of these hold; otherwise it is `Allow once? [y/N]` as
before:

- the call is `exec` or `execute`, the two tools F2 names (the request carries no risk
  category; a rule for any other tool would never match, so the failure mode is asking again);
- `call.input.command` is a string with at least one word and no separator character;
- the daemon advertises `permission.rules` and `permission.set_rules`.

```text
[approval] exec: Tool `exec` requires approval for starting processes under the workspace-write preset
  command: cargo test -p dim-cli
Allow once, always, or deny? [y/a/N] a
Always allow `cargo test -p dim-cli` (exact). Press Enter, or type its leading words for a prefix rule: cargo test
[rule saved] allow prefix `cargo test`
[tool] exec
[done] exec
```

`y`/`yes` allows once and `a`/`always` asks for the rule; every other answer, including an
empty line, denies. At the rule prompt Enter keeps the exact rule; the command's leading words
make a prefix rule (all its words make the exact rule); other text repeats the rule prompt. EOF
and Ctrl+C keep their meaning: EOF denies and closes input, Ctrl+C stops the turn.

```text
 CLI                                    App Server
  | approval.request {exec, command}  <--|
  | user: a, rule words                  |
  | permission.rules ------------------->|  {revision r1, rules}
  | permission.set_rules(r1, rules+rule)->|  {revision r2}
  |    permission_rules_conflict: read again and retry (at most 3 attempts)
  |    other failure: [rule not saved] ..., then "Allow once? [y/N]" for the same request
  | approval.respond allow ------------->|  Approval rechecks; the rule now answers
  |                                      |  identical later calls without asking
```

The rule and the answer are separate writes. A rule that already exists is not written again
(`[rule present]`). If the request is cancelled while the write is outstanding, the CLI reports
the rule's outcome and sends no answer. After Ctrl+C it starts no further write: a pending read
ends as `[rule not saved]`, and the stop settles the request. The recheck after allow (F2 §5.3)
still applies, so a deny rule written meanwhile wins. Command text is shown through the terminal
filter.

### Decisions

1. **No default preset in the CLI.** A default for new Sessions is user host configuration
   that every client creating Sessions must honor: Web, ACP, paired devices and automation as
   well as `dimcode`. A CLI file or environment variable would make the default depend on the
   client and become a second source to migrate. The durable place is the daemon's user
   Settings, read by `session.create`, which is a server change: requirement R1. Until then
   `--permission` (or a shell alias) selects the preset per invocation.
2. **No `--allowed-tools`/`--disallowed-tools`.** Tool names belong to the daemon profile and the
   protocol offers no catalog to validate them, so a misspelled name would be accepted and do
   nothing. `allowedTools` also bypasses the preset for every call of a tool, which presets and
   command rules already express more narrowly. The CLI keeps existing lists on every update.
3. **The CLI writes `main` only** (branch semantics above).
4. **No Shift+Tab cycle.** The key decoder belongs to the idle draft editor, which returns only
   send or exit; a running turn reads plain lines. A cycle needs an editor action result and a
   full-access confirmation inside the editor. `/approvals` covers switching.
5. **`/approvals` also adds rules.** F2 §8 lists adding as part of the panel, and with product
   storage the rules live in a database row that the user cannot edit as a file; without `add`
   a deny rule could not be created from the CLI.
6. **Retry only when the outcome does not depend on the read.** The startup flag and adding a
   rule retry conflicts; switching from the panel and removing by number report them, because
   the user chose from what was shown.
7. **Confirmation only for an interactive switch to full-access.** The startup flag is itself
   the explicit choice and works headless.

### Out of scope

| Item | What it needs |
| --- | --- |
| A reviewer model that answers approvals for the user | An approval answerer that responds through Approval's public respond operation, with its own model call and design, like the Collaboration leader route; not a CLI change |
| Allows remembered only for one Session | A recorded fact or a Session-scoped rules source (F2 §10.2 defers per-workspace user rules); a CLI-process memory would answer without a recorded decision |
| Sandbox settings in the CLI | F5 exposes `host-isolation.json` without a read or write method; HostPlatform management methods with a revision, local only, like `permission.rules` |
| Shift+Tab cycle | CLI only: an editor action result and a confirmation path (decision 4) |
| `--allowed-tools`/`--disallowed-tools` | A tool catalog read to validate names at the argument boundary (decision 2) |
| `/approvals` during a running turn | CLI only: the running turn reads plain lines and answers approvals; panel calls would join its multiplexed loop |

### Rule check

| Rule | Answer |
| --- | --- |
| Principles §1 (bounded plugins, App Server only assembles) | No new plugin or Capability; the CLI is a client of existing methods |
| Principles §3 (dependency direction) | The CLI depends on `dim-agent-types` for `PermissionPolicy`, `CommandRule` and `COMMAND_SEPARATORS`, as it already does for events; nothing depends on the CLI |
| §4 rule 1 (command events) | Policy changes go through Permission's existing `session.permission.update`, which writes `permission/changed`; rules go through their owner's typed write |
| §4 rule 2 (handover) | Unchanged |
| §4 rule 3 (before callback) | Not used |
| §4 rule 4 (core has no business meaning) | Timeline, Dispatcher and Permission are unchanged; tool names appear only in the CLI's prompt eligibility |
| §4 rule 5 (batches) | The rule write and the approval answer are separate owners' writes, ordered by the client as F2 §5.4 specifies |
| §4 rule 6 (context) | The model sees only the existing results and denials |
| §4 rule 7 (public interfaces only) | Only public methods and notifications; the CLI reads `permission/changed` from the public event stream |
| §4 rule 8 (infrastructure unchanged) | No change; two requirements are raised below for App Server owners, not infrastructure |
| §4 rule 9 (only needed facts in Timeline) | No new event; rules stay out of the Timeline |
| ADR 002 D1 to D5 | No new part, wiring, dependency, configuration or contract |
| ADR 003 | Presets and rules govern only agent tool calls; `!command` is unaffected; rules are user scope only, so no project file can widen them; replay, restore and fork confer nothing new |

### Requirements raised to mechanism owners

**R1. A user default preset for new Sessions.**
- Blocked: the feature list's permission policies and tool-approval settings need a default
  that new Sessions use. `session.create` takes its default from the daemon profile's fixed
  `session_defaults` (`workspace-write` for product and configured profiles), and no Settings
  source holds a user choice.
- Considered: a CLI file or environment variable (only `dimcode` would honor it; other clients
  and automation would create Sessions with another default); a field in the `permission`
  source (right owner, but it changes the F2 document and `session.create`, a server change).
- Needed: a user-scope value, never project scope, that `session.create` uses when the request
  omits `permissionPolicy`, managed by local-only revisioned methods like `permission.rules`.
  An existing Session never reads it again: its `SessionConfig` is fixed at creation.
- Ordering, batches, interruption, reload: none; it is read once at creation and is not an event.
- Owner: App Server (Sessions service and the `permission` Settings owner); tracked in
  [#257](https://github.com/arcships/dim/issues/257).

**R2. Agent members follow the leader branch's current policy.**
- Blocked: "a member works under the leader's policy" holds only until the leader changes its
  preset. Collaboration copies the leader Session's `SessionConfig.permissionPolicy`, so after
  `/approvals read-only` new members still start with the creation preset.
- Considered: the CLI updating each member (it cannot see members' creation and would race it);
  creating Sessions only through the flag (does not cover later changes).
- Needed: when Collaboration creates a member, it copies the leader branch's dispatched
  `PermissionState.policy` (Permission's public state) instead of the creation configuration.
- Ordering: the member takes the policy dispatched when it is created; later leader changes do
  not reach existing members, as for child branches.
- Owner: Collaboration (App Server); tracked in [#257](https://github.com/arcships/dim/issues/257).

## Implementation and verification

Changes stay in `crates/dim-cli`: `args.rs`/`main.rs` select commands,
`session.rs` reuses Session execution and its shared paged-history reader,
`display.rs` keeps human rendering separate from JSON output, and `export.rs`
formats conversations and writes new files. `permission.rs` holds the permission operations any
CLI client can call, without terminal input or output: reading and switching the `main` preset
with version checks, listing, adding and removing command rules with revision checks, and the
"always allow" steps (the eligible command, the proposed rule, the rule write as request and reply
steps for a turn loop that multiplexes its connection, and the allow answer).
`permission_command.rs` is the line-mode `/approvals` command, and the approval prompt stays in
`session.rs`; both only show results. Conversation formatting belongs to the CLI. No App Server,
Capability, core lifecycle, protocol, storage migration, dependency or runtime registration changes
are required.

Reuse `cli_flow.rs` and its actual daemon profile fixture to cover `exec` and
resume, stdout/stderr separation, public JSON notification preservation,
turn/approval failure and stop settlement, fixed-watermark multi-page export,
both formats, existing-file rejection, and interactive command interception.
Cover relative paths when the CLI and Session directories differ, literal `~`
usage, child-branch omission/markers, failed or cancelled child creation, and
Unix file privacy.
Use isolated homes only. Implementation requires the focused CLI flows and
affected Clippy.

Permission controls are covered at both boundaries. Scripted daemon flows check the
`session.create` policy, the resume read/update with a conflict retry, argument errors, the
`/approvals` listing, switch confirmation, conflict reports, adding and removing rules with the
new numbered list printed after each, two removals in a row where the second number refers to the
list the first printed, the "always" prompt with a prefix rule, a conflict retry, a failed write
that falls back to answering once, an existing rule that is not written again, Ctrl+C during the
write that abandons the rule, and a command with a separator that is not offered "always". Flows against the built App
Server (run by CI with `DIM_TEST_APP_SERVER`) check that `exec --permission full-access` runs a
process tool without a question, that default `exec` still exits 3, that `exec resume
--permission` changes the resumed branch, and that "always" writes an exact rule after which the
identical call runs without asking, until `/approvals remove` deletes it.
