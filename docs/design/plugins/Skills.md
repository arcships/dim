# Skills: repository preview and selective installation

Status: Draft. Delivery status is tracked in the issue, not here.

## Target contract

Scope: process, Session and branch. Issue: #2022.

The owners are `SkillsCatalog` (process), `Skills` (Session), `SkillsBranch` (branch), `SkillContent` (Session content instance) and `SkillContentInstances` (the group template). The existing installation design below is `SkillsCatalog`'s current behavior; the discovery, enablement and Session view sections of the Skills capability stay as [Skills](../../architecture/rust-agent/plugins/Skills.md) describes.

### Use and scope: responsibility split

`SkillsCatalog` owns:

- **Roots and enablement.** The user, plugin and project Skill roots, the enabled set, the project table, the Settings sources `skills` and `skills-projects` (through the Settings owner), and the `catalog_changed` generation.
- **Installation.** Preview, install, update, remove and the install records and files; the source fetch and the user root.
- **Content registration.** The Skill root a plugin package carries, the holding plugin and the issues value.
- **Reads.** The Session view value `query(cwd)` and the management methods `skills.catalog`, `skills.set_enabled`, `skills.refresh`, `skills.preview`, `skills.install`, `skills.update`, `skills.remove`, `skills.installations`.

`Skills` owns:

- **The Session view.** The list, descriptions and errors folded from `SkillsCatalog.query`, published as `SessionSkills`.
- **Content instances.** The `SkillContent` rows it mounts and unmounts through `SessionScope`, and the `skill` tool, prompt source and `skills` submit check registered with the Session's registries.

`SkillsBranch` owns:

- **The list.** `skillsBranch.list` returns the Session's published `SessionSkills` value; the branch part does not re-query the catalog.
- **The Prompt adoption.** When the list changes, it asks the Prompt owner to adopt the current list; it does not own the Prompt state.

`SkillContent` is one immutable content instance; `SkillContentInstances` is the group template. Neither has a public surface.

Callers keep:

- The Skill's text, its source format and its repository selection; the installation selects and writes files, it does not author content.
- The model's `skill` tool use and the prompt composition; the Session part contributes the tool and the prompt source.
- The client UI around the catalog and the install preview.
- Permission and approval of a tool call; the `skill` tool is an ordinary tool.

| Requirement (callers) | Compose from existing operations without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| Register a package's Skills root | No: the root registry and issues are `SkillsCatalog`'s state | `register`, `withdraw`, `plugins`, `issues` |
| Edit the catalog, enablement or projects | No: the merged view and the Settings write are the owner's state | `skills.catalog`, `skills.set_enabled`, `skills.refresh` |
| Preview, install, update or remove a Skill | No: the fetch, selection, files and records are the owner's state | `skills.preview`, `skills.install`, `skills.update`, `skills.remove` |
| Read the Session's Skills view | No: the Session owns the folded view and its generation | `SkillsCatalog.query` + `catalog_changed`; `Skills` folds `SessionSkills` |
| Mount a Skill's content in a Session | No: the instance is a Session resource with its own lifecycle | `SessionScope.mount` with the `skill-content` group; `Skills` owns the instance |
| List the current Skills on a branch | No: the branch must read the Session's published value, not re-query the catalog | `skillsBranch.list` |
| Adopt a changed list into the Prompt | No: the Prompt owner owns the composition; the branch requests it | the Prompt owner's configure operation |
| Load a Skill from the `skill` tool | No: the tool result and the content lookup are the Session's view | the `skill` tool registered by `Skills` |

### Caller example

`PluginManager` registers a package's Skills root:

```rust
let issues = skills_catalog.register(ctx, plugin, dir, document).await?;
report_issues(plugin, issues); // the package is installed; the root is refused or partly scanned
```

The local client previews and installs:

```rust
let preview = skills.preview(source, selection, cancelled.clone()).await?;
let installed = skills.install(preview.token, selection, cancelled).await?;
```

`Skills` mounts one content instance:

```rust
let content = skills_catalog.content(skill_id).await?;
let instance = session_scope.mount("skill-content", content.id, content.data, cancelled).await?;
tool_registry.register(ctx, "skill", skill_tool(instance));
prompt_registry.register(ctx, "skills", skill_source(instance));
```

`SkillsBranch` reads the list and asks the Prompt owner to adopt it:

```rust
let list = session_skills.list(); // the published value
if list.generation != seen {
    prompt.configure(list);
}
```

### Own contract

#### Dependencies

`SkillsCatalog` uses Settings (`register`, `read`, `change`, `authorize_project`, `authorized_projects`), FileStorage (`root_path`, `write`, `remove`, `list` for install files), and the management-methods mechanism of #2095. HostPlatform (#2032) has no target contract; the source-fetch and user-home dependency is recorded on #2032 and no interface is defined here. `Skills` uses SessionScope (`mount`, `unmount`, `config`, `identity`, `activated`, `closing`), `ToolRegistry`, `PromptRegistry` and `SubmitChecks` (#2026), and the SkillsCatalog's private `query`/`catalog_changed` operations. `SkillsBranch` uses the branch's Prompt owner and the Session's published value. `SkillContent` is mounted through SessionScope; `SkillContentInstances` is its group template.

#### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `skillsCatalog.register` | Register a package's Skill root | `{plugin, dir, document}` | `{}` or `{issues}` | Accepted write is not cancelled | `invalid_manifest`, `unreadable`, `settings_write`, `settings_conflict`, `closed` |
| `skillsCatalog.withdraw` | Withdraw a package's root | `{plugin}` | `{}` | Accepted write is not cancelled | `closed` |
| `skillsCatalog.plugins` | The packages holding Skill roots | `{}` | `{plugins}` | None | `closed` |
| `skillsCatalog.issues` | The scan issues of a package | `{plugin}` | `{issues}` | None | `closed` |
| `skillsCatalog.query` | The Session's view source for a cwd | `{cwd}` | `{skills, generation, errors}` | The read can be cancelled | `unavailable`, `closed`, `cancelled` |
| `skills.catalog` | The client catalog | `{project?}` | `{skills, projects, invalid}` | The read can be cancelled | `invalid`, `closed`, `cancelled` |
| `skills.set_enabled` | Enable or disable a Skill or project | `{id, enabled, project?}` | `{}` | Accepted write is not cancelled | `missing`, `invalid`, `conflict`, `closed` |
| `skills.refresh` | Rescan the roots | `{}` | `{generation}` | None; the scan is the owner's | `closed` |
| `skills.preview` | Preview a local or Git source | `{source, selection}` | `{token, entries, invalid}` | The read can be cancelled; the token expires | `invalid_source`, `fetch_failed`, `too_large`, `cancelled`, `closed` |
| `skills.install` | Install the selected entries | `{token, selection}` | `{installed}` | Accepted work is not cancelled; it settles or restores | `expired`, `invalid_selection`, `exists`, `conflict`, `write_failed`, `closed` |
| `skills.update` | Update installed entries | `{id, revision?}` | `{installed}` | Accepted work is not cancelled; it settles or restores | `missing`, `unmanaged`, `conflict`, `write_failed`, `closed` |
| `skills.remove` | Remove installed entries | `{id}` | `{}` | Accepted work is not cancelled; it settles or restores | `missing`, `unmanaged`, `delete_failed`, `closed` |
| `skills.installations` | The install records | `{}` | `{installations}` | None | `closed` |
| `Skills` view | The `SessionSkills` value and its generation | `{}` | `{list, generation}` | The Session close stops the read | `unavailable`, `closed` |
| `skill` tool | Read one Skill's content for the model | `{name}` | tool result content | The tool call's cancel | `unknown`, `disabled`, `unavailable`, `invalid` |
| `skills` check | Check operator input before it is written | `{input}` | context or block | The entry's cancel | `block`, `warnings`, `proceed` |
| `skillsBranch.list` | The branch's current list | `{}` | `{skills}` | The branch close stops the read | `unavailable`, `closed` |
| Prompt adoption | Ask the Prompt owner to adopt the list | `{list}` | `{}` | The branch close stops the request | the Prompt owner's errors |

The `SkillsCatalog`'s management methods are registered through #2095; the `skillsCatalog` data face carries the four content operations. The `query` and `catalog_changed` are the private Session-facing read; no other owner calls them. The `Skills` Session part has no public service.

#### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid_manifest`, `invalid_source`, `invalid_selection` | Fix the package, source or selection; nothing was written |
| `unreadable`, `fetch_failed`, `too_large` | Report the source error; the catalog is unchanged |
| `missing`, `expired`, `unmanaged` | Re-read the catalog or preview; the entry is gone or not owned |
| `exists`, `conflict` | Re-read and decide; the write did not happen |
| `write_failed`, `delete_failed` | The accepted work could not settle; report the partial state and its recovery |
| `settings_write`, `settings_conflict` | Retry after Settings is available, or report the conflict |
| `unknown`, `disabled`, `unavailable` | Report the Skill view or tool error |
| `block`, `warnings`, `proceed` | The check's result; `block` stops the input |
| `cancelled` | Nothing was committed; the read or preview stopped |
| `closed` | The owner is unloading; use the replacement generation |

#### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| Content register/withdraw | Validate the root, scan it within the limit, write the Settings document, update the registry | Accepted write completes | The plugin row unloads; its root is withdrawn | New registrations refused; the next generation reads the documents | Settings write and registry by `SkillsCatalog`; scan by `SkillsCatalog` |
| `query` / `Skills` view | Scan the roots, fold the enabled list, publish `SessionSkills` and its generation | The read stops; the last value stays | The Session's view ends; the branch sees unavailable | The Session's view is rebuilt from the next catalog generation | Scan by `SkillsCatalog`; view by `Skills` |
| Preview | Fetch to a bounded staging area, parse, return a token and entries | The fetch and staging stop; the token is discarded | The preview token expires with the caller | Staging is cleaned up | Staging by `SkillsCatalog` |
| Install/update/remove | Validate the selection, write the files and records, settle the batch | Accepted work is not cancelled; it finishes or restores | The caller's row unload does not undo an accepted batch | In-flight batches settle; the next generation reads the records | Files and records by `SkillsCatalog` |
| Content instance | `Skills` mounts the `SkillContent` child after `activated`, registers the tool and prompt source | A dropped mount unmounts the child | The Session's instances unmount | New mounts refused; every instance unmounts | Instance by `Skills` through `SessionScope.unmount` |
| `skill` tool | Resolve the content instance and return the text | The tool call's cancel stops the read | The row unload stops its tool | Pending calls end `closed` | Content by the instance; result by the tool caller |
| Branch list | Read the published `SessionSkills`; no catalog query | The read stops | The branch's row unload stops it | The next value is published by `Skills` | Value by `Skills`; read by `SkillsBranch` |
| Prompt adoption | The branch asks the Prompt owner to configure the list | The branch close stops the request | The branch row unload ends the adoption | The Prompt owner's state decides | Prompt by the Prompt owner; request by `SkillsBranch` |

### Exceptions with reasons

- `SkillContent` is an immutable content instance mounted through `SessionScope`; its lifecycle is the Session's. `SkillContentInstances` is the group template; neither has a public name.
- Install/update/remove do not cancel after acceptance: the files and records must finish or restore; the caller sees the batch settle or the recovery error. The design §6 wording about cancellation is corrected by this contract.
- `SkillsCatalog.query` and `catalog_changed` are a private Session-facing read, not a public face. `Skills` owns the folded view; `SkillsBranch.list` is the public list.
- `SkillsBranch` asks the Prompt owner's configure operation; the old `ConfigureControl` interface is deleted. The branch part does not own the Prompt state.
- `skills.install`'s token is a value with an expiry; it is not a handle and carries no authority.
- The `skills.*` methods are management methods (#2095), not a second `skillsCatalog` face.

### Client-visible changes

None. `skills.*` keep their client-visible behavior; `skills.install/update/remove` already never cancel after acceptance and settle or restore; `SkillsCatalogBinding` and `ConfigureControl` were plugin-facing; the `Skills` view and `skillsBranch.list` keep their values.

### Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | `SkillsCatalog` process; `Skills` Session; `SkillsBranch` branch. The branch part folds the published list and writes no Timeline event; it is not a Capability. |
| §2 scopes | Installation and enablement are process; the view, content instances and tool are Session; the list read is branch. |
| §3 orthogonality | The Session part reads the catalog's private value; the branch reads the Session's published value; no catalog re-query in the branch. |
| §4 rule 1 | No install or catalog command event; the Settings and install records are the owners' state. |
| §4 rule 2 | No handoff: content instances are mounted by `Skills`; tokens and records are values. |
| §4 rule 3 | Installs and previews run in the owner's tasks; the Session's view is published as a value. |
| §4 rule 4 | The vocabulary is roots, entries, installations, generations and views; no client or package business appears. |
| §4 rule 5 | No batches. |
| §4 rule 6 | Skill text reaches the model only through the `skill` tool result or the prompt source; no other path. |
| §4 rule 7 | Callers use `skillsCatalog`, `skills.*`, `skill`, `skillsBranch.list` and the Prompt configure; no caller holds `SkillContent`, `SkillContentInstances` or the catalog's private state. |
| §4 rule 8 | Requirements to #2080, #2026, #2027, #2032, #2093, #2094 are listed below. |
| §4 rule 9 | No Timeline fact. |
| §7 callbacks and ids | The view generation, install token and installation id cross as values; content instances stay with the Session. |
| §7 one change | The old `SkillsCatalogBinding`, `ConfigureControl` and public Rust-only management methods are removed in the same change that migrates every caller. |
| ADR 002 D1 | Process `SkillsCatalog`; Session `Skills`/`SkillContent`; branch `SkillsBranch`. |
| ADR 002 D2 | No wrapper: `skillsCatalog` is on `SkillsCatalog`; `skillsBranch` on `SkillsBranch`. |
| ADR 002 D3 | Extensions depend on the SkillsCatalog; the Session part owns its content. |
| ADR 002 D4 | Roots and Settings sources are structural; no assembly business setting. |
| ADR 002 D5 | Operations, values and ids are language-neutral; the Rust binding is the appendix. |

### Requirements raised to mechanism owners

- #2080: FileStorage `root_path`, `write`, `remove` and `list` for the install files under the user root; no new operation.
- #2026: `ToolRegistry.register` must accept the `skill` tool source; `PromptRegistry.register` must accept the Skills prompt source; `SubmitChecks.register` must accept the `skills` check.
- #2027: the Prompt owner's configure operation must replace `ConfigureControl`; the branch passes the current `SessionSkills` value.
- #2032: HostPlatform's target contract is pending; the requirement comment records the intent (user home value and source fetch with cancellation); no interface is defined here.
- #2093: SessionScope `mount`/`unmount` with the `skill-content` group and the `config`/`identity`/`activated`/`closing` values.
- #2094: Settings `skills`/`skills-projects` sources and `SkillSettingsHost`; the existing operations cover the read and write.

### Binding appendix

`skillsCatalog` is on `SkillsCatalog`; `skillsBranch.list` on `SkillsBranch`; the `skills.*` methods are registered through #2095. `SkillsCatalogBinding` does not exist. `Skills`' tool, prompt source and check are private registrations; `SkillContent` is a private instance mounted through SessionScope.

```rust
#[rutis::service(name = "skillsCatalog")]
impl SkillsCatalog {
    pub async fn register(&self, caller: &Ctx, plugin: String, dir: String, document: SkillDocument, cancelled: CancelledFn) -> Result<SkillIssues, SkillContentError>;
    pub async fn withdraw(&self, caller: &Ctx, plugin: String, cancelled: CancelledFn) -> Result<(), SkillContentError>;
    pub fn plugins(&self) -> Vec<String>;
    pub fn issues(&self, plugin: &str) -> SkillIssues;
}
```

`SkillDocument` and `SkillIssues` are the types in `skills/catalog.rs`; `SkillContentError` and `SkillListError` are `#[serde(rename_all = "snake_case")]` enums serialized as `{code, data}` with `message` in `data`; the wire `code` is the variant name and matches the error table. `From<CallError>` keeps the callback's own `code` and `message`. `CancelledFn` is the binding's cancellation argument, never a request field. `skillsBranch.list` takes `caller: &Ctx` and returns the current `SessionSkills` value.

`SkillsBranch`'s list is a data face; the request is empty and the reply is the current `SessionSkills` value. A branch part may call any data face of the branch group with `line_owner::<T>(line)`, except registration and lifecycle services (ADR 027); `SkillsBranch` calls the Prompt owner that way. The install and management operations are the typed requests and replies in `skills/catalog.rs`; the install token and installation id are values.


Scope: process.
Issue: [#740](https://github.com/arcships/dim/issues/740).
Rules: [design rules](../../architecture/rust-agent/principles.md), [ADR 002](../../adr/002-plugin-scope.md).


## Supersedes

Replaces the old Skills.md §2 public operations and errors, §3-§8 and the installation API; the `skillsCatalog` and `skillsBranch` rows in [service-faces.md](../service-faces.md); the Skills rows in [resource-owner-operations.md](../contracts/resource-owner-operations.md) and [session-boundary.md](../contracts/session-boundary.md); `SkillsCatalogBinding`, `ConfigureControl` and the public Rust-only management methods. Keeps the source/revision format, install/update/remove behavior and `/skill:` input. Rules: [§7 所有者与操作](https://github.com/arcships/dim/pull/2029). Dependencies are defined by [FileStorage #2080](https://github.com/arcships/dim/pull/2080), [Settings #2094](https://github.com/arcships/dim/pull/2094), [SessionScope #2093](https://github.com/arcships/dim/pull/2093) and the Session registries #2103; Prompt is #2102; HostPlatform is pending #2032.

## Installation format and behavior
### 2.1 Source and revision

A source is a local directory or a Git repository, acquired exactly as PluginManager acquires a
plugin directory: `https://` or `ssh://` only, no submodules, no Git hooks, no terminal prompt, no
local or `ext` transport, and a tree in which no symbolic link leads outside it ([PluginManager
§7](PluginManager.md#7-security)). A local source must be an absolute path
that resolves to a directory and passes the same link check.

Each operation owns one directory, `<home>/.agents/.staging/<random>`, outside every scanned root
and on the same file system as the user skills root: the acquired tree under `work`, the
installation being prepared under `place` and the one it replaces under `old`. The whole directory
is deleted when the operation settles, however it ends; one that an abnormal end left behind is
deleted when the process next starts. The directory is created only by an operation that needs it:
install and update always, preview only to clone a Git source, so previewing a local directory
writes nothing. Apart from that directory, an installation creates and deletes nothing outside the
user skills root.

Every reply and error is a safe projection: a Git URL is reported without user information, query
or fragment, and Git's own error text is never forwarded. The record keeps the URL as given, like
`plugins.json` does.

The **revision** is SkillsCatalog's content identity of a Skill directory: a digest over every
regular file under it, by relative path and content, in sorted order. A path component named
`.git` is skipped, and symbolic links are never followed — a selected Skill that contains one is
refused — so a bundled script or template changes the revision exactly as a `SKILL.md` change does.
Each file is read in chunks, and a file larger than 8 MiB is not digested at all: the preview
reports it as an issue and the install refuses the Skill that holds it, so a bundled asset cannot
grow the process. An installation's revision is the digest over its ordered selection of
source-relative path and per-Skill revision, so the same tree yields the same installation
revision whether it came from a path or a clone, and an unchanged branch is not stale.

### 2.3 Install

```json
{
  "source": {"path": "/src/acme-skills"},
  "id": "acme-skills",
  "skills": [{"path": "writing/review", "revision": "sha256:…"}],
  "overwrite": false
}
```

Order:

1. Validate the request shape at the boundary: source shape, `id` (`[a-z0-9_-]{1,64}`), a
   non-empty selection of distinct paths.
2. Acquire the source and scan it. A selected Skill whose files now digest to another revision is
   `skills_preview_stale`; the reply names the path and the revision found, and nothing else
   happens.
3. Check the selection against the scan: every path is one the scan found and directly contains
   `SKILL.md`; no two selected Skills declare the same name; no user-root Skill outside this
   installation provides one of those names; no selected Skill contains a symbolic link.
4. Check the destination `<user skills root>/<id>`. Only a directory the record owns is replaced:
   an id the record holds needs `overwrite`, and a directory or file at `<user skills root>/<id>`
   that no record owns is `skills_install_unmanaged`, reporting the Skills found there, because the
   user root is also where hand-written Skills live. Nothing outside `<user skills root>/<id>` is
   ever written or deleted.
5. Copy the selected Skill directories, keeping their source-relative paths, into `place`; rename
   the existing `<user skills root>/<id>`, when there is one, to `old`; rename `place` to
   `<user skills root>/<id>`. A replacing install keeps its id and replaces the selection as a
   whole; the previous directory is deleted with the operation directory.
6. Commit the record through the SkillsCatalog's record (the Settings document), which writes the Skills business document
   with revision-conflict retry.
7. Rescan the user root, advance the catalog version and announce it, so a Session sees the Skills
   before the reply.

A failure in 2–5 leaves the previous installation and the record untouched. A failure in 6 removes
the new files and restores the previous directory. The reply carries the installation it replaced
under `previous` (absent when it created one), so the caller can show what changed.

`id` is optional. Without it, the owner derives it from the source's last path component
(`owner/repo` → `repo`), ASCII-lowercased, characters outside `[a-z0-9_-]` replaced by `-`,
truncated to 64, empty → `skills`. The derivation never reads user information from a Git URL, so
a credential-bearing and a clean URL of the same repository derive the same id. A derived id that
is recorded for a different source is `skills_install_exists`, and the user names one explicitly.

### 2.4 Update

`skills.update` takes an installation id and re-acquires the recorded source: a Git source at its
recorded ref (the default branch when none was recorded), a local source at its recorded path.
The recorded selection must still exist in the new tree; a path that disappeared refuses the
update with its name and changes nothing, because dropping a Skill the user chose is not an
update. The installation's revision and Git commit advance; the id and selection do not. The reply
carries the record before the update under `previous`, so the caller can show whether anything
changed: equal revisions mean the source had not moved. Update is also the repair for a recorded
installation whose directory was removed by hand.

The record read before the source is acquired is what the update may replace. The install compares
it with the record under its lock: one that another client removed, or replaced with another source
or revision, while the source was being acquired refuses with `skills_install_unknown` and changes
nothing, so a slow clone cannot undo a removal or a newer installation.

### 2.5 Remove

`skills.remove` takes an installation id, deletes `<user skills root>/<id>` after the same shape
check the owner applies to its own directories (exactly that path, real directories, no symbolic
link component), removes the record, rescans the user root and advances the version; its reply is
the new catalog version. A recorded installation whose directory is already gone removes its
record only. Unrelated Skills, other installations and plugin-owned Skills are untouched.

If deleting the files fails, the record is kept and the reply says so, whether or not every file
was removed: the record is the only proof of what the installation owns, so it cannot be dropped
first. The user retries; the next update reinstalls the files.

## 9. References load with the operator's input

An operator names a Skill with a `/skill:<name>` token in the input. That reference loads the
Skill deterministically, before the model is asked, with the same input: no new input, no model
round trip, and no AgentLoop change. The design is:

- **The mechanism is `SubmitChecks`.** The Skills Session part registers the check `skills` with
  the Session's `submitChecks` ([Hooks A.6.2](Hooks.md#a62-submit-check-at-the-operator-input-entries)),
  the table Hooks' `UserPromptSubmit` uses. `session.submit` and the steer of
  `session.input.withdraw` call it before writing, and the blocks it returns are appended to the
  operator input's `user` content after the operator's own content, so the block is part of the
  input that drives the request and is recorded in the Timeline. The check is named `skills`, so
  its blocks follow `hooks`' in name order. `session.branch_create` returns no blocks: a
  branch's first input is built from the prompt alone.
- **The token.** A reference is `/skill:` followed by the name: one unbroken token, with no space
  after the colon. A token starts the message or follows whitespace, so it can stand
  mid-sentence, and it never starts inside a word, so `a/skill:x` and `https://host/skill:y`
  reference nothing. A name is one or more characters from `A-Z`, `a-z`, `0-9`, `_`, `-` and
  `.`, and it never ends with a full stop: a trailing `.` ends the token and stays the
  operator's text, so "Review it with /skill:review." references `review` and keeps its full
  stop, while a full stop between other characters stays in the name (`/skill:a.b` is `a.b`). A
  Skill whose name is spelled any other way, or ends with a full stop, cannot be referenced, and
  the palettes do not list it. Names are matched exactly against the enabled Skill names.
- **The token is the operator's words.** The message text stays as typed and whatever follows the
  token reaches the model unchanged: there are no argument placeholders and nothing is rewritten.
  A draft that begins with `/skill:` is sent as a message and never run as a command, and the
  `skill:` namespace keeps the token apart from the built-in commands, so no collision rule is
  needed. `/` stays the only trigger and `@` stays the file picker.
- **One grammar, one definition.** `dim_agent_types::skill_token` states the token: where it may
  start, which characters a name holds, and the spans it occupies in a text. The check, the CLI
  composer, the web composer and the CLI, web and ACP transcript projections all read that one
  definition, so every surface agrees on what loads and on what is drawn as one unit. The token
  is drawn as one highlighted unit, or a chip, in the input and in the transcript, and a
  transcript never repeats its prefix as prose. `dim-agent-types` is the crate the server and
  every client already share as values; the web mirrors the same grammar in
  `web/src/skill-token.ts`, with a test that pins the same cases.
- **A drawn token is one unit wherever it was typed.** The spans are byte ranges of the text the
  operator typed, so a surface marks the characters whose source byte lies in a token rather than
  counting bytes per row: a wrapping that drops a break, expands a tab or joins a CRLF must not
  move the highlight off the characters. A surface that already maps a byte range onto what it
  paints uses that mapping directly; one that wraps the text itself carries each drawn
  character's source byte out of the wrap and shares that one wrap with the plain rows.
- **A filled reference shows its name, and only its name.** The CLI input draws a reference the
  palette filled as the Skill's name (`/status`), and holds `/skill:<name> ` as its text: the name
  is what the operator chose and what they read back, while the token stays what is submitted
  and kept in the input history. The name is a projection over that text and never a
  rewrite of it, so a Skill loads identically whether it was drawn by name or typed in full, and
  text the operator typed is always drawn as typed. A name that is also a command the input
  dispatches keeps the canonical token in the draft, because the plain name would read as that
  command while the draft is still a message. The CLI's selection and copying behavior is
  defined in [CLI commands](../cli-commands.md#command-menu); other surfaces state their own presentation.
- **`$skill: <name>` keeps loading.** Inputs and habits from before this token keep working, so
  the parser accepts the old marker too: `$skill:`, optional spaces, then the run of
  non-whitespace characters that follows, matched exactly. No client writes it any more and the
  documentation no longer describes it; the parser and its test are its only remaining statement.
- **Lookup is the `skill` tool's view.** The check reads `SessionSkills::current()`: the enabled,
  user-invocable Skills of the Session's current view. A disabled, unknown or non-user-invocable
  name adds nothing, and a view error, a stopped part or a reloading part adds nothing: an error is
  no opinion. Each Skill is resolved once, in the order of its first reference. References are read
  from the operator's own content only: a block that starts with a runtime marker, which includes
  the blocks every submit check adds, never adds one, so a steer of a queued input loads the same
  Skills as its submit.
- **Two invocation flags.** A `SKILL.md` may narrow who may invoke it. Both flags default to
  allowing the invocation, so existing Skills keep their behaviour, and both are read at scan time
  with the rest of the frontmatter:

  | Field | Effect |
  | --- | --- |
  | `disable-model-invocation: true` | The model may not invoke the Skill: it is left out of the model's list (`dim/skills/list`) and the `skill` tool refuses it, while `/skill:<name>` still loads it |
  | `user-invocable: false` | The operator may not invoke the Skill: it is left out of the CLI and web palettes and a token naming it loads nothing, while the `skill` tool still loads it |

  A value that is neither `true` nor `false` is an invalid manifest, reported as a `skills.catalog`
  issue and kept out of the catalog exactly as a missing `name` or `description` is: the existing
  frontmatter error handling applies, and no default fills the gap. A number is such a value: the
  frontmatter parser keeps it apart from a string, so `disable-model-invocation: 1` and
  `user-invocable: 0` are refused rather than read as the default, which would silently widen a
  mistyped restriction to both invocations. An absent key, `null`, `~` and an empty value are the
  default, because they declare nothing.
- **One block per Skill.** The block is `<dim-skill name="{name}">`, the exact text the `skill`
  tool returns, and `</dim-skill>`. The name is escaped with the attribute escaping every
  `<dim-…>` marker uses, and one function renders the loaded text for both the tool result and
  the block, so the two paths cannot drift. The block is a text block of the input's `user`
  content, never a raw block, so the input stays operator input.
- **The model-facing text says which path loaded the Skill.** The instructions state that a
  `/skill:name` token followed in the same message by a `<dim-skill name="name">` block means the
  Skill is already loaded: follow it, and do not call `skill` for it. A token without such a block
  is loaded with the tool, as today. Every other rule stays.
- **Consumers see the block where hook contexts appear.** Recall reads the input's `user` content,
  so it sees the added block after the operator's text, exactly as it sees a `UserPromptSubmit`
  context (Hooks A.6.2). A reader that wants the operator's words alone leaves it out the same
  way, as SessionTitle's title text does ([SessionTitle §0.1](SessionTitle.md#01-fields-read-for-one-title-attempt)).
- **Clients show one line.** A text block of an operator input's `user` content that is not the
  first block and that `dim_agent_types::markup::runtime_element` names `dim-skill` is shown as
  one line naming the loaded Skill, never as its text ([clients](../clients.md)); the same
  predicate tells a `dim-hooks` block beside it apart, which a client shows as its own row
  (Hooks §3.3). The ACP gateway applies the same rule when it
  replays an input to the editor on `session/load` and on a history read, and it delimits each
  `/skill:<name>` token in the operator's own words so the editor can draw it as one unit; it
  advertises no Skill as an ACP command, because that list carries the gateway's native
  `/compact`, `/usage`, `/goal` and `/loop` only ([AcpGateway §4.7](AcpGateway.md)). A prefill for
  edit or fork takes the operator's text only, so resending does not load the Skill twice. Export
  keeps the raw record.
- **The palettes offer the token, not a marker.** The CLI and web `/` menus list enabled,
  unshadowed, user-invocable Skills whose names the token can carry, and choosing one fills
  `/skill:<name> ` and sends nothing. Typing `/skill:` narrows the menu to Skills. Neither palette
  writes the old marker.
