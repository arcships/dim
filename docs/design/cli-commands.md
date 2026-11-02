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

## Command menu

In both scrollback and fullscreen TUI, group commands by category and sort
categories and command names alphabetically by their canonical English identifiers.
Search relevance takes priority; equally ranked matches use the same category/name order.
Exit and suspend require their exact name or alias before dispatch: choosing an
incomplete match fills the command name and keeps the composer open. Exact typed
commands, including hidden commands, retain their own dispatch behavior.
The command menu lists `/permissions` as the permission entry. `/approvals` and `/approval`
remain compatibility aliases when typed directly. Run, queue and steer keys are
omitted from the menu. The menu only lists slash commands: the view actions for
picking an earlier message and scrolling to the top or bottom are also omitted;
`Tab`, `Home` and `End` retain their bindings.

The menu fits the available terminal columns and rows in both modes. Command
names take priority over descriptions and shortcut labels; a shortcut that
would crowd out its command name is omitted from the row, without changing
the binding. On short screens, fewer commands are shown and the window follows
the selection. Scrollback keeps the command draft visible above the menu and
omits the menu title and footer when necessary; fullscreen keeps its palette
query and selection above the composer. Category grouping and ordering do not
change with terminal size.

When the usual history reservation leaves fewer than eight live rows,
live controls take priority: either renderer can use up to eight rows for
the live region, limited by terminal height. Fullscreen reduces the composer's empty padding
and the palette's query padding before overlapping either input. The draft
editor scrolls within the remaining rows and keeps its caret visible without
discarding text. Otherwise the usual history reservation is retained.

```text
Commands
  app     /exit        exit
  app     /keys        show the key bindings
  model   /permissions   file access, tool approval, process isolation and command rules
  session /rename      rename this session

/ + Enter          -> /exit (completion only)
/exit + Enter      -> exit
/permissions + Enter -> permission controls
```

```text
Fullscreen, 8 rows             Scrollback, 8 rows
+- Command Palette -------+
| >                       |   +- input -----------------+
| app /exit               |   | > /                     |
+-------------------------+   +-------------------------+
                              +-------------------------+
+- input -----------------+   | app /exit               |
|                         |   +-------------------------+
+-------------------------+   AGENT
```

## Run keys and queue

Queue and steer differ from Codex CLI: `Enter` queues while a run is in progress, and
`Ctrl+S` steers. The other run keys follow the Codex CLI interaction (checked in its TUI,
v0.166.0). `Enter` submits when no run is in progress.

```text
key                 run idle                     run in progress
Enter               submit                       queue the draft; empty draft:
                                                 steer the oldest queued message
Tab                 pick an earlier message      pick an earlier message
                    (fullscreen)                 (fullscreen)
Shift+Left          —                            empty draft: take the newest queued
                                                 message back into the draft
Esc                 keep the draft               interrupt; every queued message
                                                 returns to the draft, oldest first
Ctrl+S              —                            draft not empty: steer the draft;
                                                 empty: steer the oldest queued message
Up / Down           input history                input history
Home / End          scroll (fullscreen, empty)   scroll (fullscreen, empty)
```

- A queued message runs after the current run ends, one message per run. A
  steered message joins the current run at its next turn boundary.
- Taking a message back, and interrupting, withdraw the message through
  `session.input.withdraw`. The text
  enters the draft only after the withdrawal is deleted. A message that already left
  the queue is not put back, and its text is not sent again.
- Withdrawals of an interrupt are sent before the stop, so the run does not start the
  next queued message.
- `Ctrl+C` clears the draft. `Esc` never clears it.

### Queue strip

With a message queued, the strip above the composer draws the queue in a titled border: messages oldest first, the next to run at the top, then a hint naming the keys that act on them. During a run, `Shift+Left` on an empty draft withdraws the newest queued message into the composer to edit. `Ctrl+S` steers the draft, or the oldest queued message when the draft is empty; `Enter` on an empty draft also steers the oldest queued message. Up/Down remain input history. The hint names a command only when the command table has one for the queue, so a typed `/name` it shows runs locally instead of being sent to the model. No command views the queue, so the hint names keys only.

```text
┌─ Queue (2) ─────────────────────────────────────┐
│ › fix the failing test in session_host          │
│ › then update the design note                   │
│ shift+← edit last · ctrl+s steer now            │
└────────────────────────────────────────────────┘
```

A queued message the strip does not show adds its count to the hint: `+3 more · shift+← edit last · ctrl+s steer now`. The strip shows at most five messages and keeps the height it reached while the queue drains.

## Usage report

In both TUI modes, `/usage` prints the main branch's context usage. When the
active model uses Dim OAuth, it also queries and prints the account's plan,
credits, rolling windows and feature meters through the existing
[account usage contract](../../crates/dim-oauth/docs/runtime-contract.md).
Other model providers keep context-only output; signing in alone does not
enable the account report. The `/login` account view remains available.

Context and account failures are reported independently: unavailable context
usage does not hide account usage, and an account error does not discard the
context report. Missing optional account fields stay absent. The illustration
below uses synthetic data; an account need not have a 5h window.

```text
/usage
  Context: 1,849 / 256,000 tokens (...) ...

  DimAgent OAuth
  Pro · active · expires 2026-11-30

  Credits     25,000 / 300,000  8%
  [###-------------------------------------]

  5h window   9,500 / 10,000  95%
  [######################################--]

  Web search  211 / 1,500  14%
  [######----------------------------------]
```

## Goal

`/goal` reads the goal of the line the client shows and writes it through the methods Goal
registers. The methods, the marker blocks and the rule that reads the goal belong to
[Goal](plugins/Goal.md#a2-session-methods) (A.2, A.3) and [Pursuit](plugins/Pursuit.md#3-changes-to-goal);
this section owns what the native clients do with them. The command is listed in the palette as a
`session` command, like `/compact`.

| Form | Call |
| --- | --- |
| `/goal` | none: the goal in force on the shown line, or that it has none |
| `/goal OBJECTIVE` | `session.goal.edit` while the goal is active or paused, `session.goal.set` otherwise |
| `/goal set OBJECTIVE` | `session.goal.set` |
| `/goal edit OBJECTIVE` | `session.goal.edit` |
| `/goal pause` | `session.goal.pause` |
| `/goal resume` | `session.goal.resume` |
| `/goal clear` | `session.goal.clear` |

Every call names the shown line in `branchId` and replies with its input's `sequence`. A form that
writes nothing else (`/goal`, `pause`, `resume`, `clear`) takes no further word; otherwise the rest
of the line is the objective, spaces included. `set` and `edit` also name the objective, so an
objective that is one of the form words is written after one of them: `/goal set pause` sets the
objective "pause", and `/goal set set` sets the objective "set".

`/goal OBJECTIVE` writes `edit` only while the goal in force is active or paused: the status, the
judge and the continuation count stay, and a paused goal stays paused. With no goal, with an
objective that has no status, or with a goal that completed or blocked, it writes `set`: a finished
goal is never edited back into work, and a new objective after one starts the next goal — status
active, the count from the first continuation, and the line works toward it.

`judge` is Pursuit's and no form sends it: a goal set from the terminal is judged by the model, and
a goal an evaluator judges keeps its judge through `/goal edit`, which writes only the objective.
A `/pursue` form is Pursuit's later option (§10) and is not part of this contract.

**Reading the goal.** The client folds the goal from the branch events it already reads, by Goal
A.3's rule: the latest live `goal:objective` block is the objective, the latest live `goal:status`
block is the status, `clear` retires both, `edit` keeps the judge, and an input the InputQueue
deleted leaves the fold. It adds no query method and asks Goal no question. `/goal` with no
arguments prints that fold. Before it picks between `set` and `edit` the client reads the line up
to its published head, and after a write it reads again before printing: the state shown and the
method chosen come from the events, never from a guess taken from the reply. The fold starts over
at the line's own opening — its
`branch/start`, or the `session/forked` a fork writes after the facts it copied — so a new line has
no goal and a forked Session's `main` never shows the copied one (Goal §2, A11). A v3 client
receives that opening, so a fork reads the same here as it does in Goal.

**When Goal is not loaded.** The Session lists the methods it serves (`session.methods`). When
`session.goal.set` is not among them, `/goal` prints `Goal is not available on this daemon.` and makes
no call. A daemon that does not advertise `session.methods` is taken at its word until a call
answers `unknown_method`, which prints the same line. No form submits the typed line as a message:
a `/goal` that writes sends Goal's own change input, and Goal decides what follows.

**What the clients show.**

```text
> /goal
No goal on main.

> /goal Make the CLI tests pass
Goal set on main · active
  Make the CLI tests pass

> /goal Keep the CLI tests green
Goal edited on main · active
  Keep the CLI tests green

> /goal pause
Goal paused on main · paused (user)
  Keep the CLI tests green

> /goal
Goal on main · paused (user)
  Keep the CLI tests green

> /goal resume
Goal resumed on main · active
  Keep the CLI tests green

> /goal clear
Goal cleared on main.
```

The first line names the call that ran, the line the goal belongs to and the status in force;
`edited` is the form that found a goal and changed only its objective. `evaluator judge` follows
the status when Pursuit's judge is on the goal. A paused status names its reason: `user`,
`interrupted`, `limit`, `turn failed` or `evaluation failed`. The objective follows, one line per
line of it. A cleared goal and a line without one print one line.

The line client and both terminal UI modes print these lines; in the terminal UI they are one
report in the transcript, so earlier reports stay readable above later ones. The status area
carries one short element while the shown line follows a goal, and nothing for a completed,
blocked or cleared one:

```text
 ╭──────────────────────────────────────────────────────────────────────────────╮
 │ › _                                                                          │
 ╰────────────────────────────── ⑂ main ▸ child work · after message 0 ─────────╯
  AGENT  ⏵⏵ Ask (legacy)  ◎ goal active  1.8K / 256K              provider/model
```

Fullscreen draws the same element in the composer's bottom border, beside the run status:

```text
 ╭──────────────────────────────────────────────────────────────────────────────╮
 │ › _                                                                          │
 ╰──── ◎ goal paused   ⏵⏵ Bypass   1.8K / 256K ───── ● main · after message 4 ─╯
```

## Provider accounts

The native client signs in to every Provider whose `provider.management` entry
carries an [`account` descriptor](appserver/provider-management.md#31-read-models).
That descriptor is the only source: the client keeps no per-provider list, and
it compares no Provider id. The described accounts are the provider list; the
descriptor's `loginMethods` say which flow each one starts (`browser` or
`device`); and the calls are the descriptor's namespace's —
`<namespace>.login.begin` (device), `<namespace>.login.browser.begin`,
`<namespace>.login.browser.complete`, `<namespace>.login.status`,
`<namespace>.login.cancel` and `<namespace>.logout`. A namespace that the
daemon does not advertise is reported as unsupported instead of starting a
flow that cannot finish.

The `account` namespace is the Dim account. Its calls also carry `providerId`
and the account directory, and `/login` keeps its account panel — identity,
plan and usage, and the enterprise-email entry — because the descriptor carries
neither identity nor usage. Every other namespace is a single account whose
calls carry only the attempt id.

The terminal UI offers these operations on the `/connect` row of each described
account. A signed-out account says `Sign in`, and opening it starts its only
flow; a signed-in one shows its state and offers `Sign out`. A row whose
namespace is `account` opens the Dim account's `/login` panel instead, with its
identity, plan and usage. A Provider without a descriptor keeps the connection
form, and opening it starts nothing.

```text
/connect
╭─ Providers ──────────────────────────────────╮
│  OpenAI Codex                     Sign in    │
│  Grok Build                       Sign in    │
│› Add custom provider                         │
╰─ Enter open · Esc clear/close ───────────────╯
        │ Enter on a signed-out described row
        ▼
╭─ OpenAI Codex OAuth ─────────────────────────╮
│   Browser login                              │
│ https://…/authorize?…                        │
│   Callback URL: (paste here)                 │
│   Submit callback                            │
╰─ Select to copy · Enter opens link ──────────╯

╭─ Grok Build OAuth ───────────────────────────╮
│   Device login                               │
│   Enter code:    ABCD-EFGH                   │
│ https://…/device                             │
│   Restart device login                       │
│   Waiting for authorization                  │
╰─ Select to copy · Enter opens link ──────────╯
```

`dimcode login [PROVIDER]` starts the same flow in the line client and prints
the URL or the code the daemon returns; without a PROVIDER it lists the
described accounts. `dimcode logout PROVIDER` signs one out, and
`dimcode account PROVIDER` prints its non-secret state. `--help` lists these
forms and never contacts a daemon. A build without the Dim account provider
says so, and names the API-key Provider alternative, from each of these
commands.

## Plugin runtimes

RuntimeManager supplies the Node and Python runtimes that plugin code runs in
([RuntimeManager](plugins/RuntimeManager.md)). The daemon reads its configuration when an
action applies it (RuntimeManager §3.3), so a client can turn a runtime on, say where it comes
from and show what it is doing, without editing `<home>/runtimes/config.json` by hand and without
restarting anything. `/runtime` opens one panel in both terminal modes and the same area is
`dimcode runtime` in the line client and in the CLI.

### The panel

Rows are the two runtimes, always node then python. A row shows its name, the configured source
(`download` or `system`), the version in effect (the fixed version for `download`, the checked
interpreter's version for `system`, empty while it is `off` and nothing has been checked), `auto`
when it is on, and the status `runtime.list` gives: `off`, `acquiring`, `starting`, `ready`, `down` with its reason, or
`failed` with the reason that lists every source and its error.

Below the rows the panel names what waits on the runtime — the plugin rows that are Pending because
one is `off` or `down` ([PluginManager §2.5](plugins/PluginManager.md#25-runtimes)) — so "why is
goal not working" is answered in the same place as its fix. A failure prints one line per source,
as `runtime.list` reports it.

```text
/runtime
╭─ Plugin runtimes ─────────────────────────────────────────────────────────╮
│ › node      download  24.21.0    ready                                    │
│   python    system    3.12.15    off      not installed                   │
│                                                                           │
│   4 plugins wait for node: goal, session-title, scheduler, workflow       │
╰─ Enter install or apply · s source · a auto · r retry · Esc close ────────╯
```

```text
╭─ Plugin runtimes ─────────────────────────────────────────────────────────╮
│ › node      download  24.21.0    acquiring  12s                           │
│   python    system    3.12.15    off                                      │
╰─ Esc close · the acquisition keeps running if you close this ─────────────╯
```

```text
╭─ Plugin runtimes ─────────────────────────────────────────────────────────╮
│ › node      download  24.21.0    failed                                   │
│               https://nodejs.org/dist/v24.21.0/…: connection timed out    │
│               https://npmmirror.com/mirrors/node/v24.21.0/…: 404         │
│   python    system    3.12.15    off      not installed                   │
│                                                                           │
│   4 plugins wait for node: goal, session-title, scheduler, workflow       │
╰─ Enter retry · s switch node to system · Esc close ───────────────────────╯
```

`acquiring` shows elapsed seconds, not a percentage: the daemon reports no byte count for an
acquisition, and the panel does not invent one. A source list appears only on failure, where the
daemon already reports it.

### Menu entry

`/runtime` is a slash command in category `app`, so the menu lists it between `/keys` and
`/settings`; it needs no Session (`Needs::Nothing`) and takes no inline argument, because the panel
carries the actions. The typed subforms below are accepted when typed, not offered as menu
completions.

### Keys

The panel is keyboard-only and the same in scrollback and fullscreen, as the command menu contract
requires.

| key | meaning |
| --- | --- |
| `Up` / `Down` | move between the runtime rows |
| `Enter` | `runtime.install NAME`: apply the configuration as it stands |
| `s` | switch that runtime's source (`download` ↔ `system`) and apply |
| `a` | toggle that runtime's `auto` and apply |
| `r` | retry: the same call as `Enter`, kept because a failed row's footer says so |
| `Esc` | close the panel; an acquisition already running keeps running |

`Enter` on an `acquiring` row does nothing, which is what the daemon's `install` does while it is
acquiring. `Enter` on a `ready` row with an unchanged source does nothing either; with a changed
source it withdraws, disposes and re-acquires, as RuntimeManager §3.3 defines.

### Writes

`runtime.install {name}` is the apply the runtime already has: acquire if `off` or `failed`,
provide the launch command again if `down`, and, when the configuration's `source` changed since
the last apply, withdraw, dispose and acquire under the new source. It replies at once with the
entry, so the panel follows `runtime.list` rather than waiting for an acquisition.

`s` and `a` are `runtime.configure {name, source?, auto?}` followed by that same apply. The method
writes `<home>/runtimes/config.json` — RuntimeManager's own document — atomically, merging per
runtime and per field over the preset, and a configuration that does not write or read is the
call's `runtime_configuration` error. The panel then shows the error and keeps the row's previous
state on screen, because nothing changed.

### Plugins that wait

`/plugins` names the runtime a Pending row waits for, and `Enter` on such a row opens `/runtime`
with that runtime selected. Today the row says only `pending`.

```text
/plugins
╭─ Plugins ─────────────────────────────────────────────────────────────────╮
│ › goal           on     pending · waits for the node runtime              │
│   session-title  on     pending · waits for the node runtime              │
│   scheduler      on     pending · waits for the node runtime              │
│   workflow       on     pending · waits for the node runtime              │
╰─ Enter open /runtime · o on/off · r reload · Esc close ───────────────────╯
```

### Line client and CLI

The typed forms are the whole area, so a caller needs no panel:

```text
/runtime                              the same rows, in the scrollback client
/runtime install node                 apply the configuration for node
/runtime source node system           switch node to the system interpreter and apply
/runtime auto python on               turn python on by configuration and apply
```

```text
$ dimcode runtime list
node    download  24.21.0   ready
python  system    3.12.15   off       not installed
4 plugins wait for node: goal, session-title, scheduler, workflow
$ dimcode runtime install node
node    download  24.21.0   acquiring
$ dimcode runtime config python --source system --auto on
python  system    3.12.15   off
```

`dimcode runtime list` prints one tab-separated row per runtime with the fields the panel shows.
Which plugins wait for a runtime is `/plugins`' answer (`dimcode plugin list` in the line client), not
this command's: one place names it per plugin, and the panel repeats it only for the runtime in view. `install` and `config` print the runtime's row after the
call, which is `acquiring` or the state it returned to; a failed call prints the error and exits
non-zero. `--help` lists these forms and never contacts a daemon.

A daemon that does not serve `runtime.list` answers `unknown_method`; the panel then says the
daemon does not serve runtimes and prints the version, rather than an empty list that reads as "no
runtimes".

## Process isolation settings

`/settings` and `/permissions` expose the same `Process isolation` control. It replaces the old
`Execution` panel and separate Sandbox entry; there is no duplicate toggle, native `dimcode
isolation` command, `/isolation` command or policy file. Choices and their stored boolean mapping
belong to the [process isolation design](host-platform-isolation.md#process-isolation), not to
Permission. File access and Tool approval never change this control.

```text
Settings                         Process isolation
  File access          >       > Off
  Tool approval        >         Sandboxed · No network
  Process isolation    >         Sandboxed · Network allowed
                                 Applies to new processes and persistent shells.
```

Opening the control reads `{source: "execution", scope: "user"}` through
[`settings.read`](appserver/settings-service.md) and retains its CAS revision. If advertised,
`host.execution.backend {}` supplies `{available: true}` or `{available: false, reason}`; show the
daemon's reason without inferring availability from the client platform. The choices remain
editable. ↑↓/Tab moves, Enter selects, Esc returns. Saving uses `settings.update` with the read
revision; a conflict is shown without retrying the user's choice against unseen data. Reload
discards the draft and reads again. Loading disables mutation; unsupported methods report an
unsupported-daemon error, never a local fallback. Reset requires confirmation and writes both
booleans false. Off may retain the old inert Network value; either sandboxed choice writes its
own network value explicitly.

This is user-scope host configuration, not branch policy or the boundary of a running command.
A running process, including a persistent shell, keeps its startup boundary. An unavailable
backend still refuses the next sandboxed start as `isolation_unavailable`; displaying the reason
does not weaken enforcement. The existing backend and execution source are reused.

A model permitted to write the settings source can change the next process's boundary, just as
a user can through this control; that write creates no grant and changes no running process
([ADR 011 D2](../adr/011-execution-settings.md)). File access is not OS isolation for shell code.

## Management surfaces

A management area is one owner's management methods. The App Server serves each area through its
owner and gives clients no generic path across them ([settings
service](appserver/settings-service.md#11-what-this-does-not-do)): Skills are `skills.*`, Hooks are
`hooks.*`, MCP is `mcp.*`, the Scheduler is `schedule.*`, Workflow definitions are `workflow.*`,
command rules are `permission.rules` and `permission.set_rules`, and plugins are PluginManager's
methods. Their entry shapes differ and stay as they are; this section makes one structure out of
the client side, so an area is added once instead of once per surface.

Each area has two surfaces in the native client:

- a terminal UI panel, `/<area>`, in both TUI modes, following the [command menu
  contract](#command-menu);
- a subcommand, `dimcode <area> <verb>`.

Both surfaces use the same area adapter. `dimcode hooks list|enable|disable` keeps its output and
exit codes, except that a source's state line is worded once for the panel and the command;
`dimcode hooks edit` and `dimcode hooks set PATH` write through the same adapter.

Management text is composed in the interface language on both surfaces. The panel's text follows
`/language` from the next reply, and `dimcode <area>` uses the saved language, else the system's.

### The adapter

An adapter talks to the daemon through the client connection, so a subcommand needs no Session.
It turns its owner's reply into one client view at that boundary, it performs the area's actions,
and it refuses an action whose methods the daemon does not advertise, with an unsupported-daemon
error. That refusal names the area when the daemon does not serve its read, and names the kind
when it serves the read without the kind's method: `This daemon lists scheduled jobs but cannot
remove them.` It never falls back to a file, to another method or to the host's own environment.

The client infers nothing the methods do not report. A view that needs a fact the reply does not
carry gets that fact from the producer: `skills.catalog` reports each entry's `source`, and
`hooks.catalog` reports the user document with its revision.

A daemon older than one of those facts gets the area without it: Skills that report no `source`
are listed under the area's own heading, `skills`, and Hooks that report no `userDocument` offer
no edit. The client never derives such a fact from an id, a path or anything else, so it needs no
feature to recognise the newer contract.

### The view

Entries are grouped under source headings — user, project, `plugin:<id>`, built-in — as the
owner reports them; a hook source is itself an entry, under the heading `sources`. Each entry has a
label, a detail, its states (enabled, disabled, invalid, shadowed, read-only) and the actions it
offers. An entry its owner does not let the client write (plugin-owned, locked) offers no write
action, and its detail says why. What the owner could not use is listed after the entries as its
issues. An entry may carry the operation's input document when the run kind takes one: Ctrl+T
opens the external editor on it first, and sends what it leaves with the run.

```text
╭─ Skills ──────────────────────────────────────╮
│ >                                             │
│                                               │
│   user                                        │
│ › commit      enabled                         │
│   review      disabled  shadowed by commit    │
│   project                                     │
│   deploy      enabled                         │
│   Issues                                      │
│     /w/.agents/skills/old/SKILL.md: no name   │
│   Writes commit messages from the staged diff.│
│   /home/u/.agents/skills/commit/SKILL.md      │
╰──── Enter toggle · Ctrl+R reload · Esc close ─╯
```

### Actions

The client has a closed set of action kinds. Each kind has one key in every panel, and the hint
lists the area's kinds plus the selected entry's kinds, in the table's order.

| Kind | Key | What it does |
| --- | --- | --- |
| toggle | Enter or Space | Enable or disable the entry, as in [Execution settings](#execution-settings). |
| edit | Ctrl+E | Open the area's document in the external editor, then write it. |
| reload | Ctrl+R | Re-read the area, or ask the owner to rescan it, as `skills.refresh` does. |
| add | Ctrl+N | Create an entry. |
| remove | Ctrl+X | Remove the entry, after a confirmation, as `/models` clears a saved record. |
| run | Ctrl+T | An area's named operation, such as test, run now or start. |
| inspect | Tab | A read-only detail view. |

Esc closes the panel. An action key is a Ctrl chord, or a key the filter never takes, because
the filter takes every plain letter. A kind that acts on the area itself belongs to the area's
view rather than to one entry — reload, and add, which creates one — so it needs no selection:
the hint lists it whether or not an entry is selected, and Ctrl+R reads the area again after a
failed read. The first two areas use toggle, reload and edit. The Scheduler area introduces
add, remove, run and inspect, and the client's kinds are what it writes; a later area adds its
adapter and the methods its kinds call, and names here the kinds it adds. The hint names the run
kind's operation where an area names one: MCP's hint reads `Ctrl+T test`.

A hint the bottom border cannot hold is drawn as rows above a plain bottom border, wrapped at
the ` · ` separators; a part wider than a row wraps across rows by display width, so no key or
action is ever cut. The rows take their space from the list like the footer's, so every key
stays visible at any width, the remove confirmation included.

- add opens the external editor on a new-entry document that the adapter supplies — the owner's
  create shape, filled with example values — and writes it with the area's create method. An
  unchanged document writes nothing, and a refusal keeps the draft, which the next Ctrl+N
  reopens, as Ctrl+E does for an edit. A conflict keeps it too: a create has no revision, so
  dropping the draft's link to a stale revision is an edit's rule, not a create's. An add that
  appends an entry to a list it read, as the command rules area's does, retries the conflict
  instead, because an append overwrites nothing.
  `dimcode <area> add PATH` (`-` for stdin) is the same write without an editor.
- remove asks in the hint first: it becomes `Remove <label>? Enter confirm · Esc cancel`, and
  every key but Enter cancels without changing anything. Enter sends the area's delete method
  and reads the area again. `dimcode <area> remove ID` deletes without a prompt, because the id
  typed on the command line is the intent, as `dimcode logout PROVIDER` signs out without one.
- run sends the area's named operation for the selected entry. A run whose answer changes what the
  area lists — the Scheduler's run now joins the job's history and its last outcome — reads the
  area again and shows the answer as the message; a run the owner skipped says why. A run whose
  answer changes nothing the area lists shows the answer as a detail, as inspect does, and does
  not read the area again: MCP's test is the tools a temporary connection found. Whether the owner
  declares the method mutating does not decide this, since `mcp.test` runs the entry and still
  writes nothing the area lists. An operation that takes parameters is the area's own step, not
  this kind's: the entry carries the input document, and the run kind opens the external editor
  on it before the call.
- inspect asks the adapter for the entry's detail: a title and lines, not a view. The panel
  shows it in place of the list, the arrows and PageUp/PageDown scroll it, Esc returns to the
  list with the selection kept, and a second Esc closes the panel. `dimcode <area> inspect ID`
  prints the same lines, or the area's own verb where its design names one.

`/workflow:save` opens its area in the panel's create editor: the first view opens the editor on
the area's create document, as Ctrl+N would, and an area that does not offer add stays on its list
with the sentence the support check gives.

An entry of an area whose documents are per entry offers edit for its own document: the Save
action then carries the entry's id, and the adapter keeps the revision it read with the draft.

A kept draft belongs to the write target it was taken for: the area's document, one entry's
document, or a create. The edit key of a target reopens only that target's kept draft, at the
revision its text came from; another target's edit opens its own document and leaves the kept
draft where it is, and only the reply to a draft's own write settles that draft.

### Writes

Writes follow [Execution settings](#execution-settings):

- the adapter keeps the revision it read with the draft, and the revision is not a field the user
  types;
- a conflict writes nothing and shows the error; the area is read again, the draft's link to the
  stale revision is dropped, and the next edit opens the stored document. A read that fails after
  the conflict still drops that link and says why the area is not drawn, and the next read retries
  it. It never retries automatically and never overwrites a newer document;
- reload discards the draft and reads again;
- a conflict message states the owner's fact — what changed, and that nothing was written — and
  leaves where the text is to the surface that holds it: the panel says what the next edit or add
  does with the kept text and names its file, and the command line names the file that holds the
  text, a stdin input included, and exits 1;
- a refusal that does not involve a newer document — text that does not parse, or one the owner
  refuses — keeps the draft's revision, so the edit key reopens the user's text and writes it
  again;
- a reply that does not concern the draft — a toggle, a read, another area's reply — keeps the
  draft and its revision; only a conflict on the draft's own save, a reload of its area, or its
  own settlement drops that link;
- what counts as a write is decided by the method behind the action: a toggle, a save, and a
  Skills reload (`skills.refresh`), which the owner declares mutating; a Hooks reload is a read,
  since it calls nothing before `hooks.catalog`;
- a write the daemon accepted is reported as done: a failure of the read that follows is
  reported after the action, as a read failure, never as a refusal or a conflict. The CLI prints
  the action's line, then the read error on standard error, and exits 0; a Skills or Scheduler
  toggle names the entry from the read that follows it, and the id when that read failed, since
  the read is what supplies the name; a Hooks toggle names the workspace, since the toggle is the
  project source; a read on its own that fails is still a failure. The panel shows the action's
  message and the read error, keeps no view until the next read, and settles the draft as on
  success: its file is removed and its link dropped. An applied action keeps the owner's answer
  even when the read after it fails;
- loading and writing disable mutation.

### The external editor

- The editor is `$VISUAL`, then `$EDITOR`, then the platform default: `vi`, or `notepad` on
  Windows. On Unix it runs through `sh -c '<editor> "$@"' <editor> <file>`, as git does, so a
  value with quotes or a path with a space works.
- Each edit writes its own file, created exclusively and readable only by the user (mode 0600 on
  Unix), in a temporary directory under the product home (`<home>/edit/<area>-<unique>.json`),
  which is not the native store, the Web workspace or anything exported. A successful write, or
  an unchanged draft, removes its file; a draft that is kept stays until the user removes it,
  and every message that keeps one names it.
- The terminal UI suspends while the editor runs — raw mode and the alternate screen are left
  and restored — and works in both TUI modes.
- If the file is unchanged, nothing is written. A non-zero editor exit writes nothing either:
  the message names the status and the file that keeps the user's text.
- If the text does not parse, or the owner refuses it, the error is shown and the edit key opens
  the editor again with the user's text.
- An interactive edit needs a terminal. Every edit has a non-interactive twin, `dimcode <area>
  set PATH` (`-` for stdin), which writes with the revision it reads immediately before the
  write.
- A document can hold commands, environment and headers. The temporary file has the same
  exposure as the user's own settings file, and nothing more.

### The session port and the one panel

- Management goes through one generic request and one generic reply on the session port, each
  carrying the area and a typed action or view: `Command::Management(ManagementRequest)` and
  `Update::Management(ManagementReply)`.
- One panel type draws every area. It is built on the shared list panel
  ([`ListPanel`](../../crates/dim-cli/src/ui/list_panel.rs)), and its confirmation, busy and
  message state live in one place.
- Adding an area means an adapter, its command menu rows, its subcommand verbs and its design
  rows. It adds no new `Prompt`, `Command` or `Update` variant.
- `/plugins` keeps its own panel. `/permissions` keeps its panel and its keys, and its Command
  rules row opens the area below; the three controls are choices among values, not entries, so
  they stay pages of the one panel.

```text
/skills ──► Command::Management { area, List } ──► adapter
                                                    │ methods advertised?  no ──► unsupported-daemon error
                                                    │ skills.catalog
                          Update::Management ◄───────┴ group, entries, states, actions, issues
Panel  ◄───────────────────{ area, view }◄──────────┘
   │
   ├─ Enter ──► Command { area, Toggle { id, enabled } } ──► skills.set_enabled ──► skills.catalog
   │                                                                                (the view again)
   └─ Ctrl+E ──► the external editor ──► Command { area, Save { id?, expectedRevision, document } }
                                          ──► hooks.save ──► hooks.catalog

dimcode skills enable ID ──────────────────────────────► the same adapter, over its own connection
dimcode hooks set PATH   ──────────────────────────────► the same adapter, over its own connection
```

### The first two areas

Skills (#814): the `/skills` panel and `dimcode skills list|enable|disable|reload` over
`skills.catalog`, `skills.set_enabled` and `skills.refresh`; `/reload-skills` runs the reload
action with no panel and reports what it found. An entry is enabled or disabled by its id, so a
toggle needs no revision of its own. Installing and removing a Skill package stays with plugin
installation ([overview](overview.md#appendix-d-cli-layer)), which owns it.

Hooks (#809): the `/hooks` panel and `dimcode hooks list|enable|disable|edit|set PATH`. The panel
lists each source as an entry with its state; a toggle enables or disables the project source
through `hooks.set_enabled` (a project source starts disabled, and only the user can enable it), and
edit opens the user document in the editor and writes it with `hooks.save`, which is the CAS write
of the Settings source `hooks`. `dimcode hooks set PATH` (`-` for stdin) is the same write without
an editor, and either one keeps the user's text in its own file under `<home>/edit/` when the
revision is stale. The user source offers edit and no toggle, because it is always in effect; a
plugin's source offers neither, and its detail names the plugin.

### The Scheduler area

Scheduler (#815): the `/scheduled` panel, `/schedule` its alias in both modes, and `dimcode
scheduled list|enable ID|disable ID|edit ID|set ID PATH|add PATH|remove ID|run ID|history ID`
over `schedule.list`, `schedule.save`, `schedule.delete`, `schedule.set_enabled`,
`schedule.run_now` and `schedule.history` ([Scheduler §A.3](plugins/Scheduler.md#a3-management-methods)).
Every job is an entry: its name is the label, and its schedule, next window and last outcome are
its detail. A job whose schedule the owner cannot use is marked invalid and its detail carries the
owner's message; a job the scheduler holds while it is disabled says it has no next window. The
owner reports no grouping, so the jobs share one heading, `jobs`.

A job's document is the record the owner wrote; the client changes only the fields `schedule.save`
takes — `name`, `schedule`, `prompt`, `cwd`, `model`, `mode`, `policy`, `max_run_minutes`,
`enabled`, `session_id` and `agent_id` — and carries the id on the Save action and the job's
revision as the revision the draft was read at. `schedule.save` is a patch merged over the stored
job, so an edit writes only the members the draft changed: the adapter reads the job again,
refuses a draft whose revision is no longer the job's, and sends the changed members with the id
and the revision. A draft that changes no editable member writes nothing, and a member the draft
deletes cannot be cleared because the patch has no null: the write is refused before it is sent
and names the member. The exception is a `session_id` whose `mode` becomes `create`, which the
owner clears itself. A stale revision is `schedule_conflict`, the
typed conflict: nothing is written, the jobs are read again, and the next edit opens the current
record. `created_by`, `policy_cap` and `enabled_at` are the owner's to derive and are never sent.
A new entry starts from the owner's create shape — a name, an example `every` schedule, a prompt
and the Session's workspace — and writes with `schedule.save` without a revision. `inspect` is
`history`: the job's recent runs, newest first, with the state the owner recorded.

```text
╭─ Scheduler ──────────────────────────────────────────────────────────────────────────────────────────────────────────╮
│ >                                                                                                                    │
│                                                                                                                      │
│   jobs                                                                                                               │
│ › nightly-build   enabled                                                                                            │
│   release         disabled  invalid                                                                                  │
│                                                                                                                      │
│   every 60 minutes                                                                                                   │
│   Next run 2026-11-02T09:00:00.000Z.                                                                                 │
│   Last run completed at 2026-11-01T09:00:12.000Z.                                                                    │
╰───── Enter toggle · Ctrl+E edit · Ctrl+R reload · Ctrl+N add · Ctrl+X remove · Ctrl+T run · Tab inspect · Esc close ─╯
```

Ctrl+T runs the selected job now and reports what the owner answered — `launching`, or the reason
the run was skipped — and Tab shows the job's history in place of the list:

```text
╭─ Recent runs of nightly-build ─────────────────────────────╮
│ 2026-11-01T09:00:12.000Z  completed                        │
│ 2026-11-01T08:00:05.000Z  skipped (overlap)                │
│ 2026-10-31T09:00:03.000Z  completed                        │
╰───────────────────────────────────── ↑↓ scroll · Esc back ─╯
```

`dimcode scheduled add PATH` (`-` for stdin) is the create without an editor, `dimcode scheduled
history ID` prints the lines the detail shows, and `dimcode scheduled remove ID` deletes without
the panel's confirmation.

### The Workflow area

Workflow (#816): the `/workflow` panel, `/workflow:save` its create editor in both modes, and `dimcode
workflow list|enable NAME|disable NAME|edit NAME|set NAME PATH|add PATH|start NAME [PATH]|history
NAME` over `workflow.list`, `workflow.save`, `workflow.start` and `workflow.status`
([Workflow](plugins/Workflow.md#a3-management-methods)), plus the Settings client methods the
enablement needs. Every definition is an entry, grouped under the source the owner reports:
`user`, `project` and `plugin:<package id>`. A user definition is always enabled and its document
is the entry's document; a project definition starts disabled and is a file in the repository; a
package definition is installed and offers no write. An entry the owner cannot use is marked
invalid and its detail carries the owner's issues.

A project definition's enabled state is the `projectEnabled[cwd][name]` member of the user
Settings document. The owner's own service refuses to change it (`userOnly`), so the toggle is a
client write: `settings.read`, the member set on the document as read, then `settings.update` at
the revision the read reported. The document is copied as read, so a definition, a limit or
another workspace's map is never lost. `settings_conflict` is the conflict path: nothing was
written, the definitions are read again, and the message says so.

Add writes either source from one document, `{source, document}`, with `source` `user` by
default; a project save creates or replaces `.dimcode/workflows/<name>.yaml` and stays disabled,
and the note says so. Edit is offered for user definitions only: the list's Settings read carries
the document, the save carries the revision the panel read, and a `conflict` answer means the
definition changed while the editor was open: nothing is written, the area is read again, and the
draft keeps the user's text. The CAS unit is the whole user document, so a save also answers
`conflict` when another definition or an enablement changed while the editor was open. A save may
not rename the definition: the owner keys by the document's name, so a renamed edit would save a
second definition and leave the old one, and the area has no remove; the write is refused before
the call and the draft is kept. A project definition is edited wherever it lives, like a project
hook document, and the entry's detail names its path.

Run is offered for enabled, valid definitions of any source. A definition that declares parameters
opens the editor on one JSON object, one key per parameter: its `default` where the definition
gives one and an empty string where it does not. The document is read back as an object of strings
whose keys the definition declares; anything else is refused with `Parameters are a JSON object of
strings.` and nothing starts. An empty string is a supplied value, not an omission. A definition
with no parameters starts at once. The start carries the entry's name and source and the
workspace; from the panel it also carries `report_to {session_id, branch_id}` for the Session and
the line the transcript shows, the line a submitted message goes to, which is how `/tasks` finds a
management-started run when it is next opened. A panel with no Session yet, as after `/new`
before the first message, starts a run that reports nowhere. The command line has no Session, so
`dimcode workflow start` sends no `report_to` and its runs are outside `/tasks`. The note is
`Started <name>: <run_id>.`. In the run document the panel opens, a parameter left at the value
it was opened with is not sent, so the owner applies the definition's full default, while a
document from a file or stdin is sent as given.

Inspect reads `workflow.status {}` and filters the retained runs to the entry's name and source.
A project definition belongs to one workspace, so its runs are filtered to this workspace's `cwd`
as well: another workspace's definition of the same name is a different definition. A user or
package definition is the same everywhere, so its runs come from every workspace and each line
names the run's `cwd`. The detail is the definition's lines, a `Recent runs` line and up to ten
runs, newest first. The area has no remove: the owner has no delete method, so a user definition
leaves only by editing the Settings document, and a project definition by deleting its file.

```text
╭─ Workflow ────────────────────────────────────────────────────────────────────────────────────╮
│ >                                                                                             │
│   user                                                                                        │
│ › review-and-fix   enabled                                                                    │
│   project                                                                                     │
│   nightly-check    disabled  invalid                                                          │
│   plugin:kit                                                                                  │
│   release-notes    enabled  read-only                                                         │
│                                                                                               │
│   Analyze, fix in parallel, summarize                                                         │
│   Saved in the user workflow source; always enabled.                                          │
╰─ Ctrl+E edit · Ctrl+R reload · Ctrl+N add · Ctrl+T run · Tab inspect · Esc close ─────────────╯
```

`dimcode workflow list` prints one row per definition (name, source, state, description) and the
issues of the invalid ones; `enable` and `disable` address the project definition of that name;
`edit` and `set` address the user definition and refuse a project name with the file to edit;
`add` is the create without an editor; `start NAME [PATH]` takes the parameters from PATH, or from
stdin for `-`, or starts with none; `history NAME` prints the detail's lines. A bare name resolves
the way `workflow.start` resolves it: an enabled definition first, then any definition, and within
each pass project, then user, then package. Project definitions start disabled, so a bare name still
names one. A command run in a workspace that Settings does not know prints the owner's
`definition_source_failed` message and exits 1. A listing the owner cut at one payload prints the
definitions it holds and says the later ones are omitted.
### The MCP area

MCP adds no kind. Its entries are the effective catalog — the servers merged by id with project
last — grouped `user`, `project` and `plugin` in that order, and by id within a group; an entry's
label and id are its id. The panel and `dimcode mcp` use toggle, edit, reload, add, remove, run
and inspect over `mcp.catalog`, `mcp.configuration`, `mcp.update`, `mcp.save`, `mcp.remove`,
`mcp.set_enabled`, `mcp.test` and `mcp.inspect` ([MCP management](appserver/mcp-management.md)).

A toggle writes the source's enablement at the revision `mcp.catalog` reported immediately before
it, so the compare-and-swap covers that window; it acts on one id, so a change elsewhere in the
source is not overwritten. A user entry's enablement is a local record, and a project entry starts
disabled because only the toggle or `dimcode mcp enable|disable` turns it on. An edit reads
`mcp.configuration` for the entry's revision and its document, then writes `mcp.update`, never
`mcp.save`: a save takes a complete configuration, and the read withholds the secrets, so a save
round trip would erase them.

The edit document is the patch: an omitted field keeps its stored value, a present value replaces
it, and `env` or `headers` changes only the keys it names. The seed names the command and working
directory, empty `env` and `headers`, and a configured OAuth's reported fields; it never names a
stored argument, environment value, header value, bearer token or URL, because those are withheld
or wholesale-replaced. The document carries no transport, since the stored entry's transport is
fixed. A stdio patch names the command, the working directory and the environment keys it
changes:

```json
{"command": "npx", "cwd": "/work", "env": {"TOKEN": "value"}}
```

A remote patch does the same for the endpoint and the headers. Both are withheld from the seed,
so a patch that changes them supplies them: an omitted `url` or `headers` key keeps what is
stored.

```json
{"url": "https://mcp.example.com/mcp", "headers": {"X-Key": "value"}}
```

Add writes `mcp.save` with the catalog's source revision — `userRevision`, or `projectRevision`
for a source of `project` — and refuses an id the catalog already lists, since a save would replace
that entry. The create document is one entry with its source and the definition `mcp.save` takes:

```json
{
  "id": "example",
  "source": "user",
  "configuration": {
    "type": "stdio",
    "command": "npx",
    "args": ["-y", "@modelcontextprotocol/server-everything"],
    "env": {},
    "enabled": false
  }
}
```

The two remote forms replace `configuration` with
`{"type": "http", "url": "https://mcp.example.com/mcp", "headers": {}, "enabled": false}` or the
same with `"type": "sse"`, and add `"auth": {"type": "bearer", "token": "…"}` for a static
credential or `"oauth": {"clientId": "…", "callbackPort": 1456, "scopes": "…",
"authServerMetadataUrl": "…"}` for OAuth; the two exclude each other, and OAuth also excludes an
`Authorization` header. The editor has no transport picker, so the document shows stdio and the
document says how to switch.

A run is `mcp.test`: it executes or contacts the stored entry once and writes nothing the area
lists, so its answer is a detail — the tools a temporary connection found, titled `Tools of <id>` —
and the area is not read again. An inspect is `mcp.inspect`: the effective entry and its Session
connections, titled `MCP entry <id>`, and never written back. A plugin entry's configuration has no
revision, so it offers no write, carries `read-only` and says its plugin owns it; test and inspect
still serve it. Sign-in, sign-out, the callback and disconnect are a state machine or a
connection-only action, not a kind, so they stay line commands and verbs.

```text
╭─ MCP ────────────────────────────────────────────────────────────────────────────────────────────────────────────────╮
│   user                                                                                                               │
│ › docs      enabled                                                                                                  │
│     user · stdio · signed in                                                                                         │
│     Executable: npx                                                                                                  │
│     Stored arguments (hidden): 2                                                                                     │
│     Working directory: —                                                                                             │
│     Environment names: DOCS_TOKEN                                                                                    │
│   project                                                                                                            │
│   metrics   disabled                                                                                                 │
│   plugin                                                                                                             │
│   notes     enabled  read-only                                                                                       │
╰───── Enter toggle · Ctrl+E edit · Ctrl+R reload · Ctrl+N add · Ctrl+X remove · Ctrl+T test · Tab inspect · Esc close ─╯
```

### The command rules area

Command rules (#819): the Permissions panel's Command rules row and `dimcode permissions
list|add PATH|remove RULE` over the user's command rules ([permission
controls](#permission-controls), [command rules](appserver/permission-command-rules.md)). The
owner reports the list and its Settings revision with `permission.rules`, and replaces the whole
list at that revision with `permission.set_rules`; a stale revision is
`permission_rules_conflict`.

The row is appended after Process isolation, so the existing rows keep their positions. It opens
this area's panel, and Esc there returns to the Permissions panel; the three controls above it
keep their own pages.

```text
╭─ Permissions ─────────────────────────────────────────────────╮
│ >                                                             │
│   File access          Workspace read/write                 › │
│   Tool approval        Ask (legacy tool exceptions)         › │
│   Process isolation                                         › │
│   Command rules                                             › │
╰──── ↑↓/Tab move · Enter apply · Esc back ─────────────────────╯

╭─ Command rules ───────────────────────────────────────────────╮
│ >                                                             │
│   user                                                        │
│ › allow exact cargo test -p dim-cli                           │
│   deny prefix git push                                        │
│                                                               │
│   Matches only this command.                                  │
│   Allows a simple command that would otherwise ask.           │
│   Applies in every Session, in every workspace.               │
╰──── Ctrl+R reload · Ctrl+N add · Ctrl+X remove · Esc close ───╯
```

Every rule is an entry. The owner has no id for one, so the id and the label are the rule's value,
`DECISION MATCH COMMAND` — `allow exact cargo test -p dim-cli`, `deny prefix git push`. A rule has
a decision, not an on/off state, so the entry reports no enablement (`enabled: None`) and the
panel draws neither `enabled` nor `disabled`; it reports no states either, because a stored rule
the owner cannot use fails the whole read. The area offers three kinds: reload re-reads
`permission.rules` (the owner reads the Settings source on every call, so there is no rescan
method), add appends one rule, and remove removes the rule the entry names. Toggle, edit, run and
inspect are not offered: a rule is not on or off, the one write replaces the whole list, and a
rule's whole content is its label and its two detail lines.

Add opens the external editor on one rule, the owner's write unit without the revision:

```json
{"command": "replace-with-a-command", "match": "prefix", "decision": "deny"}
```

The command is a placeholder no one keeps, so the template reads as the example it is: the
shared create rule writes nothing for a document left as it was, and the panel says `No change.`
`deny` with `prefix` is the fail-closed example: a user who replaces only the command still denies.
The adapter decodes the document as a command rule and checks it with the line prompt's
`permission::rule` (word normalization, a non-empty command, no shell separators); the write is
`permission::add_rule`, the line prompt's own operation: read `permission.rules`, append unless an
equal rule exists, and write at the read revision. An add is the one kind that retries a conflict,
up to three attempts, because an append does not depend on the other rules and overwrites
nothing. An equal rule writes nothing and says it is already present. `dimcode permissions add
PATH` (`-` for stdin) is the same write without an editor.

Remove asks in the hint first, `Remove deny prefix git push? Enter confirm · Esc cancel`, and
writes the list without that rule at the revision the adapter read. The action carries no
revision: a rule's id is its value, so a remove acts on exactly the rule the user saw, and a
change to some other rule while the panel was open is not a conflict. Two failures write nothing
and show the list as it now is: the rule is no longer listed (`That command rule is no longer
listed; nothing was removed.`), or another client wrote between the read and the write and the
owner answered `permission_rules_conflict` (`Command rules changed since they were read; nothing
was removed.`). `dimcode permissions remove 'deny prefix git push'` takes the whole rule as one
quoted argument and deletes without a prompt, because the rule typed on the command line is the
intent.

The area is user scope only: Permission has one user Settings source, with no project rules, and a
`.dimcode/permission.json` has no effect. Nothing is cached: the panel reads the list when it
opens, on Ctrl+R and after each of its writes, so a rule written by Allow always, by the line
prompt or by Web appears on the next of those reads, not while the panel sits idle.

`dimcode permissions list` prints one rule per line, with the decision, the match and the command
separated by tabs, then the columns and what deny means. `dimcode permissions remove` takes the
whole rule as one quoted argument, so global options stay unambiguous.

## Commands

| Entry | Behavior |
| --- | --- |
| `dimcode tui` | Open the existing interactive scrollback client. |
| `dimcode acp [--agent ID]` | Serve ACP over stdin/stdout through the client-host [AcpGateway](plugins/AcpGateway.md); lifecycle and unsupported features follow its contract. |
| `dimcode exec [-p TEXT] [--image PATH ...] [--json]` | Run text and/or local images and exit; read redirected stdin when `-p` is absent. |
| `dimcode exec resume SESSION_ID [-p TEXT] [--image PATH ...] [--json]` | Continue the named persisted Session with text and/or images. |
| `dimcode session export SESSION_ID --format markdown\|json [--output PATH]` | Export the main branch; write to stdout when no output path is supplied. |
| `dimcode session rename SESSION TITLE` | Set the Session's manual title through `session.rename` and print the title the server returns. |
| `dimcode session rename SESSION --clear` | Clear the manual title, so the Session is untitled again. |
| `dimcode session delete SESSION` | Permanently delete the Session and everything the store keeps for it; there is no prompt. |
| `dimcode session cleanup (--older-than DURATION\|--keep N) [--yes]` | Show the Sessions one selector matches, then delete them after confirmation. |
| `/rename TITLE` or `/rename --clear` | Set, or clear, the shown Session's manual title; the text is a command and never becomes model input. |
| `/export markdown PATH` or `/export json PATH` | Export the current main branch to a file while the interactive client is idle. |
| `dimcode export SESSION_ID [--output PATH] [--database PATH]` | Write the Session's whole Timeline as versioned JSONL without a daemon ([Timeline export](appserver/timeline-export.md)); write to stdout when no output path is supplied. |
| `dimcode daemon vacuum [--database PATH]` | Rewrite an offline native store with SQLite `VACUUM` to reclaim the pages deleted Sessions left, and print its size before and after ([storage format](appserver/native-storage-format.md#sqlite-运行参数与空间回收)); it fails while a daemon owns the database and never creates or upgrades one. |
| `dimcode migration [run\|inspect] [--source OLD_HOME]` | Import by default, or inspect, a stopped legacy data home through the shared [Migration plugin](../../crates/dim-migration/README.md). |
| `dimcode migration list\|show TASK\|cancel TASK` | Query durable results or request cancellation without a daemon. |
| `dimcode migration retry TASK [--source OLD_HOME]` | Reconcile a previous task's source again; return a new task linked to the old report. |
| `dimcode skills list` | List the Skills of the workspace over `skills.catalog`: each entry's name, source, state, file and description, then the catalog's issues. |
| `dimcode skills enable\|disable ID` | Enable or disable one Skill over `skills.set_enabled`, then report the catalog again. |
| `dimcode skills reload` | Rescan the user and project roots over `skills.refresh` and report what it found, through the [Skills management surface](#management-surfaces). |
| `/skills` | Open the Skills panel of the Session's workspace ([management surfaces](#management-surfaces)). |
| `/reload-skills` | Run the reload action for the Session's workspace with no panel and report the entries and issues it found. |
| `dimcode hooks list [--cwd DIR]` | List the effective hook catalog over `hooks.catalog` ([Hooks](plugins/Hooks.md#a7-management-methods)): each source with its path, enablement and state, and each handler with its event, matcher, command and state. |
| `dimcode hooks enable\|disable [--cwd DIR]` | Enable or disable the project hooks of the workspace at `DIR` (the current directory by default) over `hooks.set_enabled`, recorded against the project root Settings authorizes for it. |
| `dimcode hooks edit` | Open the user's hook document in `$VISUAL`, `$EDITOR` or the platform default, and write it over `hooks.save` with the revision `hooks.catalog` reported; an interactive edit needs a terminal. |
| `dimcode hooks set PATH` | Write the user's hook document from `PATH`, or from stdin for `-`, with the revision read immediately before the write; unchanged text writes nothing. |
| `/hooks` | Open the Hooks panel of the Session's workspace ([management surfaces](#management-surfaces)): a source per entry, the project's toggle and the user document's edit. |
| `dimcode scheduled list` | List the jobs the Scheduler holds over `schedule.list` ([Scheduler](plugins/Scheduler.md#a3-management-methods)): each job's id, name, schedule, state, next window and last outcome, then the jobs whose schedule the owner cannot use. |
| `dimcode scheduled enable\|disable ID` | Enable or pause one job over `schedule.set_enabled`, then report the jobs again. |
| `dimcode scheduled edit ID` | Open one job's document in `$VISUAL`, `$EDITOR` or the platform default, and write it over `schedule.save` with the revision the listing reported; an interactive edit needs a terminal. |
| `dimcode scheduled set ID PATH` | Write one job's document from `PATH`, or from stdin for `-`, with the revision read immediately before the write; unchanged text writes nothing. |
| `dimcode scheduled add PATH` | Create a job from `PATH`, or from stdin for `-`, over `schedule.save` without a revision: the owner derives the id, `created_by` and `policy_cap`. |
| `dimcode scheduled remove ID` | Delete one job over `schedule.delete` without a prompt: the id on the command line is the intent. |
| `dimcode scheduled run ID` | Run one job now over `schedule.run_now` and report what the owner returned, or why it skipped the run. |
| `dimcode scheduled history ID` | Print one job's recent runs over `schedule.history`, newest first, with each run's state and time. |
| `/scheduled`, `/schedule` | Open the Scheduler panel ([management surfaces](#management-surfaces)): a job per entry with its schedule, next window and last outcome, and the toggle, edit, add, remove, run and inspect kinds; the Scheduler's verb for inspect is history. Jobs are the host's, not the Session's or workspace's; only a new job's default `cwd` comes from the Session. |
| `dimcode workflow list` | List the definitions over `workflow.list` ([Workflow](plugins/Workflow.md#a3-management-methods)): each definition's name, source, state and description, then the issues of the invalid ones. |
| `dimcode workflow enable\|disable NAME` | Enable or disable the project definition of that name over `settings.read` and `settings.update`, then report the definitions again. |
| `dimcode workflow edit NAME` | Open the user definition's document in `$VISUAL`, `$EDITOR` or the platform default, and save it over `workflow.save` with the revision the user document was read at; a project name names the file to edit instead. |
| `dimcode workflow set NAME PATH` | Write the user definition from `PATH`, or from stdin for `-`, with the revision read immediately before the write; unchanged text writes nothing. |
| `dimcode workflow add PATH` | Create or replace a definition from `PATH`, or from stdin for `-`, over `workflow.save`: the document names `source` (`user` by default) and `document`. |
| `dimcode workflow start NAME [PATH]` | Start the definition over `workflow.start` with the parameters from `PATH`, or from stdin for `-`, or with none; report `Started NAME: RUN_ID`, without a `report_to`. |
| `dimcode workflow history NAME` | Print the definition's lines and its recent runs over `workflow.status`, resolving the name as `workflow.start` does. |
| `/workflow` | Open the Workflow panel of the Session's workspace ([management surfaces](#management-surfaces)): a definition per entry with its source, state, parameters and issues, and the toggle, edit, add, run and inspect kinds. |
| `/workflow:save` | Open the Workflow panel in its create editor. |
| `dimcode mcp list` | List the effective catalog over `mcp.catalog` ([The MCP area](#the-mcp-area)): each entry's id, source, transport, enablement and authorization, then each source application's errors. |
| `dimcode mcp add PATH` | Create an entry from the panel's create document, `-` for stdin, over `mcp.save` with the source revision `mcp.catalog` reported; an id the catalog already lists is refused. |
| `dimcode mcp edit ID` | Open the entry's `mcp.update` patch in `$VISUAL`, `$EDITOR` or the platform default, and write it at the revision `mcp.configuration` reported; unchanged text writes nothing. |
| `dimcode mcp set ID PATH` | The same write from a file, or from stdin for `-`, with the revision read immediately before the write. |
| `dimcode mcp remove ID` | Delete one entry over `mcp.remove` at its source revision, without a prompt: the id on the command line is the intent. |
| `dimcode mcp test ID` | Run the stored entry once over `mcp.test` and print the tools the temporary connection found; nothing is saved. |
| `dimcode mcp inspect ID` | Print the effective configuration and the Session's connections over `mcp.inspect`, without contacting the server. |
| `dimcode mcp disconnect ID` | Close the entry's live connections over `mcp.disconnect`; it writes no configuration, and the entry reconnects on next use. |
| `dimcode mcp login\|logout\|enable\|disable ID` | Keep their behavior: browser or device sign-in, sign-out, and the enablement both `/mcp` clients share. |
| `/mcp` | Open the MCP panel of the Session's workspace ([management surfaces](#management-surfaces)): an entry per server, grouped `user`, `project` and `plugin`, with the toggle, edit, add, remove, test and inspect kinds. |
| `/mcp list\|login\|callback\|logout\|disconnect\|enable\|disable` | The line commands, unchanged: `list` prints the catalog and the Session's server states, `disconnect` closes one entry's live connections, and the rest sign in, complete a callback, sign out or toggle. |

| `dimcode permissions list` | List the user's command rules over `permission.rules`: each rule's decision, match and command, then the columns and what deny means. |
| `dimcode permissions add PATH` | Add one rule from `PATH`, or from stdin for `-`, over `permission.rules` and `permission.set_rules`: an equal rule writes nothing; a conflict re-reads and retries, at most three attempts. |
| `dimcode permissions remove RULE` | Remove the rule named by `RULE`, which is the whole rule as one quoted `'DECISION MATCH COMMAND'` argument, over `permission.rules` and `permission.set_rules` at the revision just read; a rule that is gone, or a raced write, writes nothing and exits 1. |
| `dimcode login [PROVIDER] [--directory URL] [--device] [--redirect-uri URI]` | Start the described account's browser or device sign-in ([Provider accounts](#provider-accounts)); without PROVIDER, list the described accounts. |
| `dimcode account PROVIDER` | Print the described account's non-secret state. |
| `dimcode logout PROVIDER` | Sign the described account out. |
| `/connect` | List the Providers `provider.management` reports; a described account opens its sign-in or sign-out step, and any other Provider opens the connection form. |
| `/exit` | Detach and exit from the idle interactive prompt. |
| `!command` / `!!command` | Call the Session's existing `shell.run` in record/private mode, including during a model turn. |
| `--permission read-only\|workspace-write\|full-access` | Select the preset of a new or resumed Session for `dimcode`, `tui`, `exec` and `exec resume` ([permission controls](#permission-controls)). |
| `/goal [OBJECTIVE \| set OBJECTIVE \| edit OBJECTIVE \| pause \| resume \| clear]` | Read and write the goal of the shown line, in the line client and both terminal UI modes ([goal](#goal)). |
| `/permissions` | Open the Permissions panel: independent File access, Tool approval and Process isolation controls, and the Command rules row that lists, adds and removes the user's command rules ([permission controls](#permission-controls)). |
| `/approvals`, `/approval` | Compatibility aliases for `/permissions`; legacy preset arguments remain supported. |

`dimcode hooks` writes only the user's hook document, and only through the edit kind: `dimcode hooks
edit` and `dimcode hooks set PATH`, and Ctrl+E in `/hooks`, call `hooks.save` on the Settings source
`hooks` of the home the daemon serves (`user/hooks` in native storage, `hooks.json` in a standalone
home). The project document is `.dimcode/hooks.json` in the repository, edited there. `dimcode hooks
enable|disable` records the workspace in the user-local `hooks-projects`, so a project file cannot
enable itself ([Hooks](plugins/Hooks.md#a7-management-methods)).

Global home, workspace, server and model-profile options retain their current
meanings and work before or after the command. `--server PATH` names a
standalone `dim-app-server` binary, which the CLI runs by its own name; the
default instead runs this executable's `app-server` subcommand, so naming this
executable is an error. A path that does not exist, a program that exits
before serving, or one that does not start serving in time is reported with the
same requirement, unless it left a [startup note](#daemon-startup-failure).
Existing login/account/logout, models, MCP and daemon commands remain
available. Existing `--prompt`, piped stdin and `--resume` calls retain their
behavior; `exec resume` also accepts the same existing global options.
Supplying the Session twice or the prompt twice is an argument error. `--json`
serves `exec` and `migration`, and `tui` requires a terminal. Help/version and
argument errors do not start the daemon.

### Daemon startup failure

All entries that start a daemon share the client startup in `connect_inner`.
On Unix, the daemon keeps only the standard descriptors 0-2 that it inherits
and closes every other inherited descriptor when it starts, so a long-lived
daemon never holds its launcher's pipes. Linux closes the range in one
`close_range` call; where that call is unavailable, and on the other Unix
systems, the daemon lists `/proc/self/fd` (Linux) or `/dev/fd` and closes
every number the listing names; when no listing can be read, it closes 3 up to
the process's descriptor limit, which is the soft limit, else the hard limit,
else `sysconf(_SC_OPEN_MAX)`, else 65,536, so no path leaves an inherited
descriptor open.
The daemon is detached and its stderr is the append-only `daemon.log`, so a
per-launch note is the only channel that does not parse history or depend on a
pipe the client may close. Before spawning, the CLI removes
`<native home>/dimcode.sqlite.startup`, where the native home is the daemon's
own home, `<home>/rust`. When the spawned daemon exits before the client
connects, the CLI reads that note (at most 16 KiB, and only a regular
file; on Unix it must also be owned by the current user and offer no group or
other permissions). On Unix that mode is the note's protection, since the
daemon writes it 0600; on Windows the client checks only the regular file, its
size and the header, and the private home's inherited ACL is the boundary. It
requires the `dimcode-startup-failure v1 pid=<child pid>` header, removes
control characters other than newline and tab from the body, and returns it as
the command's error followed by `Details: <native home>/daemon.log`. The
storage gate writes the note on a diagnosed incompatibility and names the
store, its marker, the daemon's supported format, whether the store is newer,
mismatched, unrecognized or too old, and the recovery paths ([native storage format §
startup incompatibility](appserver/native-storage-format.md#启动不兼容诊断)).
Without a matching note the embedded server reports the existing
`App Server exited (<status>); see ...`, and a `--server` binary reports that
the flag must name a standalone `dim-app-server` binary. The failed launch
removes the note after reading; a successful connection removes any note. The
CLI never prints `daemon.log`, never deletes, recreates, downgrades or rewrites
the store, and exits 1.
Development documentation continues to use `./dev` and its isolated home; the
terminal note itself names a compatible build or `--home PATH`.

```text
$ dima
Error: Cannot open native store /Users/me/.dimcode-alpha/rust/dimcode.sqlite.
  Store format: v30
  Daemon format: v29
  This daemon cannot read the store. Existing data was preserved.
  Run a build supporting v30, or use --home PATH for a separate environment.
  Details: /Users/me/.dimcode-alpha/rust/daemon.log
```

The interactive export parser reads the format token and treats the remainder
of the line as a literal path, including spaces, without shell evaluation or
quote expansion. Missing paths and unsupported formats print usage and return
to the prompt without submitting input. `/exit` takes no arguments. Only these
exact command names are intercepted; other text follows the normal input path.
Subcommands do not create a plugin command registry.

### Session titles

The manual title is the Session's own state: `session.list` and
`session.summaries` report it with its `manual` source, and later automatic
generation never replaces it ([title metadata](appserver/session-title-metadata.md),
[SessionTitle](plugins/SessionTitle.md)). `session.rename {sessionId, title}` sets
it and `title: null` clears it. Title text belongs to no branch and to no Timeline
event, so a rename adds nothing the model reads.

`session.rename` is a Session method: the client calls it directly and `hello.features` does not list it. A Session that does not serve it answers `unknown_method`, which the client reports as the unsupported-daemon error, and the typed
text is never submitted in its place. The same holds for a Session that is not
attached yet: with no Session shown, `/rename` reports that and sends nothing.

`dimcode session rename SESSION TITLE` prints the title the server returns and
`--clear` prints that the Session is untitled. Exactly one of TITLE and `--clear`
is required, so a forgotten argument is a usage error and never clears a title; a
blank or whitespace-only TITLE is missing, and is refused the same way.

`/rename TITLE` and `/rename --clear` do the same for the shown Session and report
which happened in one status line. The palette row declares the title argument, so
choosing `/rename` from the palette fills `/rename ` and waits for it; Enter on a
bare `/rename` is refused with a `needs <title>` notice and sends nothing, and
`--clear` is the only form that clears without one. Every form is a command the
client handles: the text is never queued and never submitted, including while a turn
runs. The `/resume` list reads `session.catalog` each time it opens, and falls back to
`session.summaries` on a daemon before the catalog, so it shows the new title; the header
shows the first message's topic and is unaffected. The
idle line client reads the same two forms and prints
`Use /rename TITLE or /rename --clear` for a bare `/rename`.

### Session deletion and cleanup

`dimcode session delete SESSION` removes the Session and everything its store keeps
([session deletion](appserver/session-deletion-and-fork.md)). There is no prompt: the
id on the command line is the intent. The reply says whether this call removed it
([`deleted`](appserver/session-deletion-and-fork.md#31-method)): `Deleted ID` on `true`,
and on `false` the command prints `Nothing was removed: no stored Session ID.` on stderr
and exits 1. An id the daemon holds no rows of is a successful delete that removed
nothing; that is not a usage error, a malformed command line is. A reply without the
field, from an App Server before it, reads as a delete: the rows are gone either way,
which is the state asked for.

`dimcode session cleanup` composes deletion from the same two methods, as
[§5.1](appserver/session-deletion-and-fork.md#51-cleanup-by-age-or-count) requires. It
reads `session.summaries` with `includeClosed: true`, so a Session this daemon closed is
listed too and marked `closed` in the preview: its rows are still stored, and no other
listing shows it ([session catalog](appserver/session-catalog.md#33-sessionsummaries)).
An App Server before that parameter refuses it `invalid_params`, and without it the
command cannot see a closed Session; it therefore stops before any delete, prints
`This App Server is too old to clean up Sessions safely; nothing was deleted.` and exits
1. The field it sends with every delete, `requireInactive`, lands with `includeClosed`,
so a server that refuses one refuses both, and a delete that answers `invalid_params`
before anything was deleted stops the same way. It
selects with one of two selectors and reads both from the `updatedAt` of
`session.summaries`, the Session's last activity:

| Selector | Selection |
| --- | --- |
| `--older-than DURATION` | Every Session whose `updatedAt` is at least `DURATION` ago. `DURATION` is a count and a unit `d`, `h` or `m`, such as `30d`. |
| `--keep N` | The N most recently active Sessions stay; every other Session is selected. |

Cleanup selects only the Sessions `/resume` lists by default
([classification](appserver/session-classification.md#35-catalog-visibility)): origin
`client` or `unknown`. An `internal` Session is never selected: a member goes with
its leader's delete and a launched Session belongs to its plugin. An `acp` Session is
never selected either; `dimcode session delete ID` deletes one. A Session it does not
select neither counts toward `--keep N` nor appears in the preview.

A Session with a running turn or an attached client is kept: the preview lists the
skipped Sessions after the selection under `Kept (active):`, in the same columns
followed by the reason (`running`, `N clients attached`), and no such Session is
deleted, `--yes` included. A line that failed or is closing is not a reason to keep
one: nothing runs on it, and the delete's own close settles it, so cleanup deletes such
a Session when nothing else uses it. `--keep N`
still keeps the N most recently active Sessions, and an active Session older than those
is kept as well without its consuming one of the N. `dimcode session delete ID` still
deletes an active Session on purpose: the server permits it, and this is the bulk
command's rule. The partition is the preview's reading of `running` (which covers
`main`) and `attachedClients`; every delete carries `requireInactive: true`, so the owner
checks again at the delete, on every line and with uses and holds included. A Session
the owner refuses with `session_active` is listed under `Kept (in use at the delete):`
after the deletes, in the same columns followed by the owner's reason, and a refusal is
not a failure: it does not change the exit status. A delete whose reply is
`deleted: false` means another caller removed the Session after the summaries were read:
the command prints `Already gone: ID` and counts it as neither a delete nor a failure. A
reply without the field counts as a delete, as `dimcode session delete` reads it. When
every selected Session was kept, the command prints `Nothing to delete: every matching
Session is active.`, whether the preview kept them all or the owner refused each delete,
and exits 0.

Exactly one selector is required. The command prints the selection before it reads a
key — the id, the title (`-` when untitled) and how long ago the Session was last
active, as one tab-separated line each, with `closed` after a Session this daemon
closed, drawn here with spaces — and deletes only after confirmation:

```text
$ dimcode session cleanup --keep 1
a1b2c3d4-…  Notes on the build  3d ago
5e6f7a8b-…  -                   12d ago  closed
Kept (active):
c3d4e5f6-…  Fix the parser      1d ago   running
Delete 2 Sessions? [y/N] y
Deleted a1b2c3d4-…
Deleted 5e6f7a8b-…
```

On a terminal, only `y` or `yes` confirms. Without a terminal nothing confirms, so a
script passes `--yes`, which also skips the prompt; otherwise the command refuses and
names `--yes`. A confirmed run deletes every selected id even when one fails: each
failure is reported with its id and the method's error, the command that retries it
(`dimcode session delete ID`) is printed for each failed id, and the run exits 1. A
selector that names no Session prints what it kept: `Nothing to delete: --keep N keeps
every Session.` for `--keep N`, or `Nothing to delete: no Session is older than
DURATION.` for `--older-than DURATION`; the run exits 0 either way. A selection whose
Sessions are all active prints the sentence above instead.

### Migration interaction

`dim` and the alpha package's `dima` invoke these same native commands. Bare
`migration` imports each existing distribution independently:

| Distribution | Legacy source | Native target home |
| --- | --- | --- |
| Global | `~/.dim/v2` containing `dimglobal.sqlite` | `~/.dim` |
| CN | `~/.dimcode/v2` containing `dimcode.sqlite` | `~/.dimcode` |

When both exist, both are imported. Sessions, Providers, Settings and task ledgers
remain in their own target home. The default migration command uses these paths
independently of the chat client's alpha/development home or `DIMCODE_HOME`.
`./dev migration` invokes the same routing without injecting its development home
or restarting a daemon. Development verification uses disposable user homes.

An explicit `--home TARGET` scopes the command to that one home, reading
`TARGET/v2` when `--source` is absent. `--source OLD_HOME` selects just that
source; when it names a `v2` directory its parent is the target. Other custom
source layouts require `--home TARGET` to make the destination explicit.
The shared Migration plugin still receives one resolved source/target pair per
invocation and knows no distribution directory convention.

Default `list` reads both target homes. Default `show`, `retry` and `cancel`
locate the task through the owners' task lists, including when its old source
has been removed. JSON output is one object keyed by `global` and `cn`;
each entry contains `home` and `result`, or `home` and `error`. An absent
distribution is omitted. No matching source is an error; a task query with no
matching task is an error; listing with neither target home present returns `{}`.
A failure in one
distribution is reported while the other is attempted. Cancellation stops the
remaining imports. Overall exit is 1 for any failed distribution, otherwise 2
for any partial result, 0 for success, or 130 when cancelled. Explicitly scoped
commands retain their single-home JSON result.

Inspect, run and retry require stopped legacy writers. Run and retry also require
closed native clients and use normal verified shutdown to settle an existing target
daemon. Without `--source-stopped`, a terminal shows the resolved homes and asks
once for all selected sources. Declining exits 130; scripts must supply the flag.
Source/target overlap is rejected before target access. Migration performs the data
conversion offline under the native store lease; it does not start a daemon.
The CLI binds `TARGET/rust/dimcode.sqlite`, matching its normal profile. Queries
and inspection do not open that database. Disposal drains accepted work before
exit. There is no persistent Migration selection or automatic trigger. Imported
business configuration takes effect through normal feature loading.

Open imported Global data with `dim --home ~/.dim`; the ordinary default home
does not switch to Global. `dima` uses the same migration destinations, so pass
`--home ~/.dimcode-alpha` explicitly to import into the alpha home instead.

```text
$ ./dev migration
Source: /Users/me/.dim/v2
Target: /Users/me/.dim
Source: /Users/me/.dimcode/v2
Target: /Users/me/.dimcode
Close legacy and native clients using these homes. Continue? [y/N] y
<fixed progress panel; see Migration terminal presentation>
Migration completed
Task: <task-id>
Full report: dim migration show <task-id> --home /Users/me/.dim

$ dim migration inspect --home /tmp/dim-new --source /tmp/dim-old --source-stopped
{ "database": "dimcode.sqlite", "tables": { ... }, "issues": [] }

$ dim migration run --home /tmp/dim-new --source /tmp/dim-old --source-stopped --json
{ "task": { "id": "...", "status": "partial", ... }, "objects": [ ... ] }

$ dim migration show <task-id> --home /tmp/dim-new
$ dim migration retry <task-id> --home /tmp/dim-new --source-stopped --json
$ dim migration cancel <running-task-id> --home /tmp/dim-new
$ dim migration list --home /tmp/dim-new
```

The [Migration plugin](plugins/Migration.md#终端展示) owns the fixed terminal
progress panel and summary. Interactive run/retry displays that panel and finishes
with a compact summary and report command. `--json` suppresses progress and prints
the complete JSON on stdout; non-interactive output also remains JSON, with at most
one stderr notice per step. Queries keep their JSON results. Exit codes are 0 for complete, 2 for partial, 1 for failure and 130 for
settled cancellation. A partial report names failed, conflicting, deferred and
unsupported objects and omitted fields. Ctrl+C requests cancellation and waits
for accepted writes; a second process can use `cancel TASK`. Retry
requires the stopped acknowledgement again, accepts a moved source path and keeps
the prior report. Querying results does not require Migration to remain loaded.

Client preference replacement requires stopped clients. Existing values and
recorded clears remain unchanged. Ambiguous interrupted file writes are reported
as conflicts; retry does not replay them over later user changes.

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

For every export entry, relative output paths use the CLI process's current
directory, including after resume; neither the Session's cwd nor `--home` changes
that base. The CLI treats `~` literally. Usage/help states: `PATH is relative to
the CLI current directory; ~ is literal; use an absolute path for another base`.
An invoking shell may expand an argument before the CLI receives it.

Other target commands require their owning features. F6 does not implement a
fullscreen interface, persistent RPC/ACP server, verification/repair loop,
provider editor or Session deletion/fork. `dimcode export` is the
[Timeline export](appserver/timeline-export.md) (ADR 005 D3): every final model
request is a `model/call` line in it; the request snapshot filters of the
feature list are not implemented.

## Update dialog

The existing update dialog uses the same actions in fullscreen and scrollback.
It fits the available columns and rows: long version, command and error text
wraps, with Up/Down scrolling when needed. The title and action row remain
visible. If the actions do not fit on one row, show the focused action and its
index; Left/Right and Tab/BackTab still cycle all actions, Enter confirms and
Esc closes. Wide terminals retain the existing confirmation layout when it fits.

```text
+- Update available --------+
| Version 0.12.0-alpha.124  |
| is available (you have   |
| > Later 3/3              |
| Left/Right Tab Enter Esc |
+----------- Up/Down 1-2/7-+
```

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
settlement, and exits with code 3. A [question](#questions) gets no answer
the same way: the question's branch and the turn are stopped, and the exit code is 3.
It does not change
`SessionConfig.interactive`. Sessions created as noninteractive retain Approval's existing owner-side policy (#98 D10).
`--permission` selects a legacy policy, including its file scope and risk decisions; a call that still asks is denied
the same way ([headless execution](#headless-execution)).
Other exit codes retain the native CLI contract, including 2 for turn failure
and 130 for interruption. A transport failure does not retry a submission.
Repeated `--image` arguments follow the headless attachment contract: text first,
then validated original image bytes in argument order, with paths relative to the
invoking process. An image-only invocation is valid. A submission error on a live
connection keeps the uncertain-outcome diagnostic/exit path unless its `error.data.appError`
is one of the seven [guaranteed pre-commit rejections](../../crates/dim-app-server/README.md#session-submission-errors):
`invalid_params`, `session_invalid_input`, `session_closed`, `session_build_failed`,
`service_reloading`, `unknown_method` or `input_blocked`. Those retain their ordinary error diagnostics.
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

Permission owns the branch policy seeded by `session.create {permissionPolicy}` and changed with
versioned `session.permission.get/update`. The independent optional fields and legacy semantics
are the [Permission contract](../architecture/rust-agent/plugins/Permission.md#policy-contract),
extended by [ADR 017](../adr/017-independent-permission-controls.md). Command rules remain
`permission.rules`/`permission.set_rules` ([F2](appserver/permission-command-rules.md)); file grants
remain `session.grants.add/remove` ([ADR 012](../adr/012-session-grants.md)).

| TUI control | Choices | Policy field or owner |
| --- | --- | --- |
| File access | Read only / Workspace read/write / Unrestricted | `fileAccess` |
| Tool approval | Ask / Bypass | `toolApproval` |
| Process isolation | Off / Sandboxed · No network / Sandboxed · Network allowed | Existing `execution` settings, not Permission |

Ask means every tool call needs consent unless explicitly allowed by a tool or command rule;
Bypass skips tool questions only. Neither answers a file-scope question, lifts an explicit deny
or changes the sandbox. File access is a separate gate using the existing fixed facts: Read only
denies writes even for an allowed tool; read grants never permit writes. A call requiring both
scope and tool consent uses one approval. The new TUI never labels a choice Full access or Auto;
Auto is reserved for a future actual automatic decision system.

For compatibility, policies without explicit overrides retain the following legacy behavior,
after tool lists and command rules, with the existing file checks taking precedence:

| Preset | What runs (Permission's existing decision, after `disallowedTools` and deny rules) |
| --- | --- |
| `read-only` | Reads of the workspace, system roots, Session-owned files and granted paths run, and other reads ask; Session tools run; file writes, commands, network and unclassified tools are denied |
| `workspace-write` | Those reads run and other reads ask; workspace file writes and Session tools run; writes to system roots or Session-owned files are denied; other outside writes without a matching write grant, commands, network and unclassified tools ask |
| `full-access` | Every tool call runs without asking |

A deny rule applies under every preset. An allow rule only turns a question into a run for a
simple command; it never lifts a denial. The product and configured daemon profiles create
Sessions with `workspace-write`; that default is unchanged.

### Startup selection

`--permission read-only|workspace-write|full-access` applies to `dimcode`, `tui`, `exec` and
`exec resume`, before or after the command. Another value, a repeated flag or the flag on any
other command is an argument error before the daemon starts.

The terminal UI remembers independent `fileAccess` and `toolApproval` choices in the product
home's `config.json`, alongside legacy `permissionPreset`. Missing new fields remain missing;
loading old config does not convert a preset into explicit Ask or widen its legacy denials.
This is a CLI preference for later Sessions, not a daemon-wide default for all clients.
On the home screen without an active Session, `/permissions` saves the selected field without
sign-in or Session creation. The first message creates the Session with the remembered choices,
including within the same client process. With an active Session, remember the choice only after
its versioned `main` policy update succeeds. Changing one field preserves the other field,
the legacy seed and tool lists. Process isolation uses its existing host settings source instead.

```text
Home > /permissions > File access > Read only
  -> remember fileAccess (no Session or login required)
  -> first message -> session.create {permissionPolicy: {preset: <legacy seed>,
                                                       fileAccess: "read-only", ...}}
```

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

A new Session created with the legacy flag receives `{preset}` without explicit overrides.
The flag wins over remembered CLI choices for that run. New members inherit the requesting
branch's current policy (see branch semantics). A preset-only update on resume changes only
the legacy seed and retains all other policy fields, including explicit overrides and tool lists;
explicit overrides still take precedence over that seed. The notice must show their effective
values rather than suggest that changing a preset replaced explicit controls.
The update happens before attach, so no turn submitted by this invocation runs under the old
preset. Without the flag or remembered choices, `session.create` omits the policy and a resumed
Session keeps its own. Remembered choices are supplied explicitly when applied; they never
reinterpret stored history.
A resume flag against a daemon that does not advertise `session.permission.update` fails before
attach. A conflict retry is correct here because the requested preset does not depend on what
the read returned; the read only supplies the version and the tool lists to keep.

The effective controls are shown where the Session is identified. Interactive start reads the policy with
`session.permission.get` when the daemon advertises that method; `exec` prints it to stderr only
when `--permission` was given, so its existing diagnostics are otherwise unchanged. A
`permission/changed` event that arrives live on `main`, from this or another client such as a
paired device, prints one record with the independent values and any legacy tool exceptions.

The overview and Settings use `Ask (legacy tool exceptions)`. The compact TUI scrollback status
uses `Ask (legacy)` so saved-setting notices still fit 80 columns; this is the same legacy policy,
not explicit Ask or another option.

```text
$ dimcode --permission read-only
dimcode · model: provider/model
local files: /workspace/project
session: <session-id>
File access: Read only
Tool approval: Ask (legacy tool exceptions)

> Fix the build
Assistant: ...
[permission] main: File access = Workspace read/write  <- changed by another client
```

The scrollback client draws a status line under the composer: the agent marker and the
tool-approval mode (`Ask (legacy)` for the legacy policy), the goal marker, the context use against
its window, and one slot for the state in force — a notice, a required answer or permission,
`esc interrupt`, or the background task count. The model, with its effort when both fit, is
right-aligned on the same line; the line is clipped to the terminal width.

### Headless execution

`exec` keeps its contract: an approval request is answered with the ordinary denial, the turn is
stopped and settled, and the exit code is 3. For legacy policies without explicit overrides,
`full-access` asks nothing except that explicit denies still refuse calls; `read-only` denies
writes and commands; `workspace-write` asks for commands and exits 3. Explicit Ask also asks for
otherwise in-scope reads; Bypass still leaves file-scope approvals intact. There is no automatic
approval answerer. The flag is an explicit legacy choice, so `exec` asks for no confirmation.

### Branch semantics

Permission's state for a branch is `SessionConfig.permissionPolicy` folded with the
`permission/changed` events of its ancestor prefix and then its own. The CLI writes only `main`,
the branch its permission controls target. Branch-scoped interaction follows
[Timeline branches](timeline-branches.md).

```text
session.create {permissionPolicy} --> SessionConfig.permissionPolicy
                                       |
                                       v
 main: permission/changed --> current branch policy
        |                      |
        | branch point         | sample when consuming a new member request
        v                      v
 child branch policy      new member Session policy
        |
        +-- its own member request --> sample child policy, not main's

Later updates do not reconfigure existing child branches or members.
```

- Child branches that the branch tool creates after a change inherit it. Existing child branches
  keep their policy; the CLI does not enumerate or update them. Their approval requests are
  still answered on their own branch id, as today.
- New Agent members copy the requesting branch's dispatched policy at the sampling point defined
  by [member permission inheritance](subagent-tool-approval.md#2-permission-inheritance). Existing
  members retain their policy, including on reuse. Permission-switch confirmation says that it
  affects future members created from this branch, not existing members. This is the target
  contract; delivery is tracked in #257.
- A fork copies the source's facts, so it starts with the source's current `main` policy (#26).

### Subagent tool approval

This section specifies the client interaction for [Collaboration's member-approval
route](subagent-tool-approval.md). It is a design requirement, not a claim that the route is
implemented. Member observation remains read-only ([member conversation](member-conversation.md));
answering uses the leader's approval surface and never submits directly to a member Session.

When the leader receives `approval.request` for `member_approval_allow`, the CLI queues it like
any other leader-Session approval, keyed by `(sessionId, branchId, requestId)`. Before enabling
Allow, call `session.collaboration.approval.get` with that leader request identity. Display the
returned server-owned member descriptor, not a command or explanation supplied by the leader
model. A non-main requesting branch is shown even if the user is currently viewing another line.

Scrollback and fullscreen use the same modal prompt and keyboard behavior:

```text
+-- Subagent tool approval ----------------------------------------+
| Requested by leader branch: research                             |
| Member: reviewer  Session: <member-session-id>                    |
| Member branch: main                                             |
| Tool: exec                                                      |
| Command: cargo test --workspace                                  |
| Member reason: Starting processes requires approval              |
| Leader reason: Allowing a member action requires approval        |
|                                                                |
| [Allow once]  [Reject]                                           |
| y allow once / n reject / Tab move / Enter confirm               |
+-----------------------------------------------------------------+
```

The line client presents the same fields followed by `Allow this member action once? [y/N]`.
Enter, another answer or EOF rejects under its normal approval rules. It does not offer `a`.
`Allow once` answers the **leader** request with `approval.respond allow`; Reject answers that
request with deny. Collaboration settles the original member request. Do not use the member id
from the displayed descriptor as the RPC answer target.

For original calls other than `exec`, show the original tool and its full input, prominently the
resolved `path`, URL or boundary (`sandbox`) where present. Commands, paths and ids remain verbatim;
localize labels only and sanitize terminal control characters. Wrap/scroll long detail and expose
all arguments before confirmation; do not silently truncate the action to a title or summary.
If the original command is simple, this wrapper still has no Always action: it is not that command
and must not create a command rule. No Full Access button or implicit policy update is offered in
this prompt; permission settings remain a separate user action.

| Client state | Display and action |
| --- | --- |
| Detail loading | Show `Loading member action...`; disable Allow; Reject remains available |
| Query failure/reloading | Show the error with Retry and Reject; keep Allow disabled and preserve the prompt |
| Method absent | Explain that this client/server combination cannot review the member action; offer Reject, never fall back to approving an opaque route id |
| Query returns `ended` | Close as `Member request ended`; deny the still-pending leader wrapper if necessary; no success message |
| Another leader request arrives | Queue it; do not replace the focused approval or reuse its loaded descriptor |
| `approval.resolved` / `approval.cancelled` from another client | Retire only the matching full request identity; a resolved answer is not evidence the member tool ran |
| Member cancellation while open | `session.notice` from Collaboration with kind `memberApprovalEnded` retires the matching branch/route prompt and denies the still-pending wrapper. Recheck detail on focus/accept as well |
| Leader stop / closing | Use normal stop/settlement; discard local detail state after its request ends |

Accepting Allow performs a fresh detail check before sending the answer. A request ending after
that check remains safe through server admission and route checks. The prompt's descriptor is
immutable; focus changes never change its target. A pending member request that ends between
checks is removed by the terminal notice, and cannot be approved onto another call. A terminal
notice racing a detail reply wins; an old pending reply never reopens that prompt. Reattachment rebuilds
from replayed leader approval requests and queries details again; cached pre-disconnect snapshots
are not grants.

An approval takes existing modal priority over member detail and task panels in both TUI modes;
closing the panel preserves the leader draft, selected model, selected line and transcript position.
Incoming requests do not move the user into the member Session. An adopted member ask awaiting the
leader model may be represented by its normal input/tool activity; it is not yet a user permission
prompt. Do not label the tool as approved merely because the user allowed the wrapper.

Headless `exec` keeps its existing deny/stop/exit-3 contract for a leader approval. `exec --json`
may emit the raw notifications, but adds no stdin approval protocol. Non-interactive leader Sessions
are settled by Collaboration without prompting. A terminal that loses its connection neither
approves nor cancels work by that fact alone.

### `/permissions`

`/permissions` is the visible menu entry in both TUI modes; `/approvals` and `/approval` open the
same controls as compatibility aliases, not a second preset chooser. The control applies to
`main`; each field selection writes only its dimension using the current whole policy and
version. A failed save neither changes remembered choices nor falls back to a legacy preset.

Process isolation opens the execution settings. The Command rules row opens the management panel
of [The command rules area](#the-command-rules-area), where the rules are listed, added and
removed; Esc there returns to this panel, and the three controls above the row keep their pages.
Arguments after `/permissions` in the terminal UI open that panel instead of being ignored, and
it opens holding one line that says where arguments work — the line prompt and `dimcode
permissions` — which stands above the panel's answers until the user's first key there, however
long the opening read takes; the compact grammar belongs to the line prompt.

```text
Permissions (main branch)
> File access          Workspace read/write >
  Tool approval        Ask                  >
  Process isolation    Off                  >
  Command rules                             >

File access                         Tool approval
  Read only                       > Ask
> Workspace read/write              Bypass
  Unrestricted

Hint: Read only: no writes.          Hint: Ask before tool calls.
      Tool approval still applies.        File restrictions still apply.

↑↓/Tab move · Enter apply · Esc back
```

In both fullscreen and scrollback, File access, Tool approval and Process isolation show short
explanations below all choices and actions, separated by a blank line. The block starts with
`Hint:`; each paragraph starts on a new line, wraps at word boundaries to the available width,
and aligns continuation lines after the prefix. Explanations are not selectable rows. Errors
remain distinct from these hints.

An old policy with no explicit tool override shows `Ask (legacy tool exceptions)` where
appropriate, with its existing automatic allows and risk denials explained rather than
pretending explicit Ask is active. Merely opening the panel writes nothing. Selecting Ask
explicitly replaces the legacy tool-risk behavior; selecting File access alone does not.
Increasing file access or choosing Bypass explains that explicit denies remain and process
isolation is unchanged; it does not claim every call will run. No combined permissive shortcut
appears in an approval prompt.

At the line prompt the no-argument command prints effective independent controls and numbered
command rules when supported. Preset arguments remain compatibility operations, not TUI labels.
The printed rules and revision are kept for `remove`.

```text
> /permissions
File access (main branch): Workspace read/write
Tool approval (main branch): Ask (legacy tool exceptions)
Process isolation: Off
Command rules (user scope, every Session):
  1  allow  exact   cargo test -p dim-cli
  2  deny   prefix  git push

> /permissions deny prefix git push
Rule already present: deny prefix `git push`

> /permissions remove 1
Rule removed: allow exact `cargo test -p dim-cli`
Command rules (user scope, every Session):
  1  deny   prefix  git push
```

| Form | Calls | Conflict |
| --- | --- | --- |
| `/permissions` | `session.permission.get` on `main`; `permission.rules`; existing execution settings read | none |
| File access / Tool approval selection | `get`, then `update` preserving untouched policy fields | Re-read before any retry; never replace the other dimension or tool lists |
| `/permissions PRESET` (legacy) | `get`; for `full-access` a `[y/N]` confirmation where Enter, any other answer, EOF and Ctrl+C mean No; then `update` with the read version and otherwise unchanged policy | `permission_conflict`: read again, print the current policy, write nothing; the user decides again |
| `/permissions allow\|deny exact\|prefix COMMAND` | `permission.rules`; unless the same rule exists, `set_rules(revision, rules + rule)`, then print the new list | read again and retry, at most 3 attempts: adding does not depend on the other rules |
| `/permissions remove N` | `set_rules(listing revision, listing without rule N)`, then print the new list | `permission_rules_conflict`: read again, print the current list, remove nothing |

`remove N` refers to the numbered list printed last. The listing, an added rule, a removal and a
removal conflict each print the resulting list, and only a printed list is kept for `remove`, so
a number never refers to a position the user has not seen. A form that prints no list (a rule
already present, a preset switch, an "always" answer during a turn) keeps the last printed one; if
the rules changed since, its revision makes the next `remove` fail, and the conflict prints the
current list. Without a printed list in this process, `remove` asks for `/permissions` first.
`remove N` keeps the number because it acts on the list this prompt printed and holds that
listing's revision, while `dimcode permissions remove` and the panel take the rule's value
because each reads the list immediately before the write, so there is no printed listing to
number.
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

### "Allow for this session"

For `read`, `write` and `edit` calls with a `path`, a file-scope question also offers `Allow for
this session` (ADR 012). With Read only or Workspace read/write file access, an outside read
needs scope consent. An in-scope call asking only because of explicit Tool approval Ask does
not need a file grant. The CLI sends the tool's path
unchanged to `session.grants.add {sessionId, path, kind: "file", mode}`: the mode is `read` for
`read` and `write` otherwise. The daemon resolves `~`, a relative path, `..` and symlinks against
the Session workspace and stores the canonical path; a new file is resolved by its parent and
final name. The CLI decides no path or kind locally.

```text
[approval] read: Reading outside the session workspace requires approval: /home/user/.ssh/config
  path: /home/user/.ssh/config
› Allow once   Allow for this session   Reject
```

`Allow for this session` writes the grant first, then answers `approval.respond allow`; the
recheck sees the grant. If the write fails, the CLI shows `Grant not saved: <error>` and still
allows the call once, as the user asked, so no failed store write blocks the turn. A read
outside the facts in a non-interactive run (`exec`) is denied with the ordinary refusal and exit
code 3; no new error path is added. This prompt does not list or remove grants; revocation has
no client surface yet. A grant answers file scope, not explicit Tool approval Ask; the current
answer still consents to this call once. A write grant also admits a read, a `read` grant never
authorizes a write, and a fork starts without the source's grants.

### Decisions

1. **Remembered CLI choices are not a host default.** Independent choices are saved alongside
   old config and sent explicitly by the CLI. A default honored by Web, ACP, paired devices and
   automation belongs to the daemon's user Settings: requirement R1. Do not reinterpret old
   config or existing policies to simulate such a default.
2. **No `--allowed-tools`/`--disallowed-tools`.** Tool names belong to the daemon profile and the
   protocol offers no catalog to validate them, so a misspelled name would be accepted and do
   nothing. `allowedTools` also bypasses the preset for every call of a tool, which presets and
   command rules already express more narrowly. The CLI keeps existing lists on every update.
3. **The CLI writes `main` only** (branch semantics above).
4. **No Shift+Tab cycle.** The key decoder belongs to the idle draft editor, which returns only
   send or exit; a running turn reads plain lines. A cycle needs an editor action result and a
   separate interaction design. `/permissions` covers switching.
5. **`/permissions` also adds rules.** F2 §8 lists adding as part of the panel, and with product
   storage the rules live in a database row that the user cannot edit as a file; without `add`
   a deny rule could not be created from the CLI.
6. **Retry only when the outcome does not depend on the read.** A startup preset, an independent
   field assignment and adding a rule can retry after re-reading and preserving untouched fields.
   Legacy panel preset switching and removal by number report conflicts because the user chose
   from what was shown. Execution Settings retains its inspected-revision save contract.
7. **Legacy permissive preset confirmation remains.** An interactive legacy `full-access`
   argument still requires confirmation; the startup flag is itself explicit and works headless.
   New controls describe their independent effects rather than offering that combined TUI choice.

### Out of scope

| Item | What it needs |
| --- | --- |
| A reviewer model that answers approvals for the user | An approval answerer that responds through Approval's public respond operation, with its own model call and design, like the Collaboration leader route; not a CLI change |
| Shift+Tab cycle | CLI only: an editor action result and a confirmation path (decision 4) |
| `--allowed-tools`/`--disallowed-tools` | A tool catalog read to validate names at the argument boundary (decision 2) |
| `/permissions` during a running turn | CLI only: the running turn reads plain lines and answers approvals; panel calls would join its multiplexed loop |

ACP, Hooks and Web remain legacy/preset-only clients for permission selection in this change.
Their existing preset controls do not select explicit File access or Tool approval, and their
omitted overrides retain legacy risk decisions. Extending those UIs is separate work, not a
second implementation of the TUI controls; Web's existing execution settings remain host settings.

### Rule check

| Rule | Answer |
| --- | --- |
| Principles §1 (bounded plugins, App Server only assembles) | No new plugin or Capability; the CLI is a client of existing methods |
| Principles §3 (dependency direction) | The CLI depends on `dim-agent-types` for `PermissionPolicy`, `CommandRule` and `COMMAND_SEPARATORS`, as it already does for events; nothing depends on the CLI |
| §4 rule 1 (command events) | Policy changes go through Permission's existing `session.permission.update`, which writes `permission/changed`; rules and grants go through their owners' typed writes |
| §4 rule 2 (handover) | Unchanged |
| §4 rule 3 (before callback) | Not used |
| §4 rule 4 (core has no business meaning) | Timeline and Dispatcher are unchanged; independent policy semantics belong to Permission, not the CLI |
| §4 rule 5 (batches) | The rule write and the approval answer are separate owners' writes, ordered by the client as F2 §5.4 specifies |
| §4 rule 6 (context) | The model sees only the existing results and denials |
| §4 rule 7 (public interfaces only) | Only public methods and notifications; the CLI reads `permission/changed` from the public event stream |
| §4 rule 8 (infrastructure unchanged) | No change; two requirements are raised below for App Server owners, not infrastructure |
| §4 rule 9 (only needed facts in Timeline) | No new event; rules and grants stay out of the Timeline |
| ADR 002 D1 to D5 | No new part, wiring or dependency; additive policy fields follow ADR 017, CLI preferences are local business configuration |
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
- Owner: Collaboration; tracked in [#257](https://github.com/arcships/dim/issues/257).
- Contract, sampling order, failure and reuse behavior are owned by
  [member permission inheritance](subagent-tool-approval.md#2-permission-inheritance).
- CLI permission controls update their target branch only. They do not enumerate members or
  propagate policy. Their confirmation explains the consequence stated under Branch semantics.

## Questions

The model's `ask_user` tool asks the user one to four multiple-choice questions and waits for
the answer, which becomes the call's tool result
([Questions](../architecture/rust-agent/plugins/Questions.md), #262). The line-mode CLI is one of
the clients that answer. It asks the waiting calls of every branch of the attached Session, one
call at a time in arrival order, and answers with `session.question.respond`:

```text
> Plan the service
[question] Database                             <- header, or "[question]"
1. Which database should the service use?
   1. PostgreSQL  managed, relational
   2. SQLite      single file
   0. Other (type your answer)                  <- only where custom text is allowed
Choose 1-2, 0 for other, empty to decline: 1
2. Which features ship first?                   <- multiSelect
   1. Auth
   2. Billing
   3. Search
   0. Other (type your answer)
Choose numbers, comma separated, 0 for other: 1,3,0
Other: audit log
[question answered]
[question] Deploy (branch reviewer)             <- a question of another branch
1. Where to deploy?
   1. production
   2. staging
   0. Other (type your answer)
Choose 1-2, 0 for other, empty to decline:
[question answered elsewhere]                   <- another client answered first
```

The CLI finds waiting calls in the `step/tool-call` and `step/tool-result` events of each branch:
live `session.event` notices, `main`'s history read at attach, and the history of every other
branch, read once from its first `session.state` notice. The same event can arrive live and from
history, in either order, so calls and results are matched by event identity. A call id may
come back in a later model request, so a call is named, settled and answered by its branch, the
`request_id` of its `step/tool-call` and its call id:

```text
session.event (live, every branch) ─┐
session.history (main at attach,    ├──> waiting ask_user calls ──> one prompt at a time
  other branches on first state)  ──┘    step/tool-call adds a call      │
                                         step/tool-result removes it     v
                                                          session.question.respond
                                                          {branchId, requestId, callId,
                                                           answers | declined}
```

An empty line on the first question declines the whole call; an empty or invalid line on a later
question asks it again. While a question is open, approvals, model output and shell results wait,
as they do for an approval prompt, and a question waits for an open approval.

| Reply or event | CLI |
| --- | --- |
| `{}` | `[question answered]` |
| `invalid_params` | `[question rejected] <reason>`; the call is asked again from its first question |
| `question_not_pending` before the call's result | `[question not ready; press Enter to send again]`; Enter sends the same answer |
| Another failure before the call's result | `[question error] <error>; press Enter to send again` |
| The call's `step/tool-result` before the reply, or while nothing is sent | `[question answered elsewhere]` for an answer or decline, otherwise `[question cancelled]` |
| `Ctrl+C` or EOF at a question | `session.stop` for the question's branch, which ends its other calls too, and for the turn while it runs; as for an approval prompt, a running shell command is not stopped first |

Without terminal input (`exec`, or a redirected prompt) nobody can answer. The CLI prints
`Interactive answer required; stopping. Rerun in a terminal to answer it` once, stops the branch
of every question it reads before it exits and the turn while it runs, and exits with 3. Exit code
3 now means "interactive approval or answer required".

### Terminal UI questions

The terminal UI uses the existing Questions client module and prompt rather than
a second waiter projection or protocol. It reads committed calls/results on every
branch through the existing fixed-watermark history/live merge. The call is always
addressed by its original Session and `(branchId, requestId, callId)`.

```text
 [ Database ] [ Features ] [ Confirm ]
 Question on branch reviewer
 Which features ship first?
   [x] Auth
   [ ] Billing
       Type your own answer: audit log
 Space toggle · Enter next · Tab switch · / commands · Esc decline
```

Reuse the current prompt's selection, review and narrow-screen window: number keys
choose, Space toggles, Tab/Shift+Tab or Left/Right moves between questions, and Enter
on Confirm submits. Custom text is available only with `allowCustom`; it is sent
separately from offered labels. Non-empty text on the custom row checks it as it
is typed (replacing the chosen label in a single choice) and clearing it unchecks it;
while editing, Esc leaves the editor keeping the text and Enter keeps it and moves to
the next question. Outside the editor, Esc declines the whole call. Display sanitization
never changes the labels sent to the owner. The composer's draft is preserved.

When the question tabs do not fit on one row, show only the current tab's label
and its position, including the Confirm tab. Tab/Shift+Tab and Left/Right keep
their existing switching behavior; tab labels cannot overflow the prompt border.

A pending question does not block commands. While its free-text answer is not being
edited, `/`, Ctrl+O and Ctrl+P open the command palette over it, drawn where that mode
draws the palette, and the question keeps its tabs, selections and typed answers. The
palette opens command entry, which keeps the keys through the command's argument, shown
on the composer line; Enter runs the command as it runs during a reply, and command
entry lasts until that Enter, Esc, or a draft that stops being a command. Esc in the
palette and while an argument is typed closes command entry and gives the keys back to
the question, declining nothing and interrupting no run. A command that opens its own
panel puts the question aside and brings it back, with its state, when that panel
closes; a question the Session stopped asking meanwhile is not brought back. `/` is the
palette until the free-text row is edited, so an answer may itself start with a path:
Enter, Space, the row's number or any other typed character edits the row, and `/`
typed there is text.
Esc keeps its meaning on every other surface: it declines an open question and
interrupts a busy run with no prompt open, with no timing window.

While submitting, disable duplicate responses. The successful reply or a committed
batch result closes the prompt; invalid/not-pending/other failures keep the answers
with a diagnostic and an explicit Confirm retry (Esc retries a decline). A result before a late reply wins;
neither a reused call ID nor another call in the same model request reuses old UI
state. Approval/question prompt order follows the existing modal behavior; a leader
question closes a member detail through its existing cleanup, and a member's question
never becomes a leader response control. Ctrl+C follows the existing TUI interrupt/
exit policy; when it interrupts, address the question's branch and any running leader
turn before a separate shell run. Line-mode and no-terminal policies are unchanged.

The [CLI contract](../../crates/dim-cli/README.md#questions-in-the-terminal-ui)
records the delivered interaction. Acceptance exercises the actual TUI and NDJSON
boundary for structured answers, retry, reused identities, branch restoration,
member isolation and draft preservation; an actual App Server/model flow verifies
that the next model request reads the answer through `ToolResult.content`.

## Implementation and verification

Changes stay in `crates/dim-cli`: `args.rs`/`main.rs` select commands,
`session.rs` reuses Session execution and its shared paged-history reader,
`display.rs` keeps human rendering separate from JSON output, and `export.rs`
formats conversations and writes new files. `permission.rs` holds the permission operations any
CLI client can call, without terminal input or output: reading and updating the `main` policy
with version checks, listing, adding and removing command rules with revision checks, and the
"always allow" steps (the eligible command, the proposed rule, the rule write as request and reply
steps for a turn loop that multiplexes its connection, and the allow answer).
`permission_command.rs` is the line-mode `/permissions` command, and the approval prompt stays in
`session.rs`; both only show results. `question/mod.rs` holds what any client needs for questions,
without terminal input or output: the `ask_user` input rules, the waiting calls derived from
events, the `session.question.respond` params and what a reply means. `question/line.rs` is the
line-mode prompt that `session.rs` feeds with notices and replies. Conversation formatting belongs
to the CLI. Independent controls use Permission's additive policy contract (ADR 017) and the
existing execution Settings contract; no core lifecycle, storage migration or runtime registration
change is required.

Reuse `cli_flow.rs` and its actual daemon profile fixture to cover `exec` and
resume, stdout/stderr separation, public JSON notification preservation,
turn/approval failure and stop settlement, fixed-watermark multi-page export,
both formats, existing-file rejection, and interactive command interception.
Cover relative paths when the CLI and Session directories differ, literal `~`
usage, child-branch omission/markers, failed or cancelled child creation, and
Unix file privacy.
Use isolated homes only. Implementation requires the focused CLI flows and
affected Clippy.

Verify permission controls at both boundaries. Reuse scripted daemon flows for the
`session.create` policy, the resume read/update with a conflict retry, argument errors, the
`/permissions` listing, legacy aliases and confirmation, conflict reports, adding and removing rules with the
new numbered list printed after each, two removals in a row where the second number refers to the
list the first printed, the "always" prompt with a prefix rule, a conflict retry, a failed write
that falls back to answering once, an existing rule that is not written again, Ctrl+C during the
write that abandons the rule, and a command with a separator that is not offered "always". Flows against the built App
Server (run by CI with `DIM_TEST_APP_SERVER`) should check that `exec --permission full-access` runs a
process tool without a question, that default `exec` still exits 3, that `exec resume
--permission` changes the resumed branch, and that "always" writes an exact rule after which the
identical call runs without asking, until `/permissions remove` deletes it.

The independent-control acceptance cases are: an in-scope read asks with explicit Ask; a tool
allow answers only the tool question; Bypass still asks outside file scope; Read only denies a
write even with a tool allow; one call needing scope and tool consent prompts once; a read grant
cannot authorize a write; an explicit deny wins with Bypass; old policies/config retain exact
legacy behavior without silent writes. Verify each choice changes only its own field, remembers
it only after a successful policy update, works before Session creation, and reports conflicts or
unsupported servers without widening. Check both TUI modes show only `/permissions` in the menu,
both aliases work, and Process isolation has no duplicate Execution/Sandbox entry. Reuse process
flows for the boolean mapping, CAS failure, unavailable-backend reason, and a process started
before an isolation change retaining its old boundary. These are requirements, not a claim of
implementation or verification completion.

Questions are covered by scripted daemon flows (`cli_flow/question.rs`): single and multiple
choices, custom text, a decline, invalid lines, inputs the handler rejects and other tools that
ask nothing, deferral in both directions with an approval, a shell result that waits, a call id
used again, a call id that a later request reuses arriving live before the history page holding
the earlier call and its result, a child-branch question read from two history pages and answered
with its `branchId`, a duplicate live event, every reply in the table above, results that close
the prompt before or during the reply, Ctrl+C and EOF before and after the turn settles, a reply
drained after Ctrl+C, and `exec` exiting 3 for questions on `main`, on a child branch and after
the turn settled, while a result read before its call asks nothing.
