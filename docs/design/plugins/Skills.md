# Skills: repository preview and selective installation

Status: Draft. Delivery status is tracked in the issue, not here.

Scope: process.
Issue: [#740](https://github.com/arcships/dim/issues/740).
Rules: [design rules](../../architecture/rust-agent/principles.md), [ADR 002](../../adr/002-plugin-scope.md).

This document owns one part of the Skills owner: how a Skill source that is not a Dim plugin
package is previewed, installed into the user scope, updated and removed. Skill discovery,
precedence, enablement, the Session view and the `skill` tool are the Skills capability's
existing contract ([Skills](../../architecture/rust-agent/plugins/Skills.md)); plugin packages
that carry Skills are PluginManager's ([PluginManager §3.7](PluginManager.md#37-content-registration)).

## 1. Scope and non-goals

The owner is SkillsCatalog, the process part that already scans the user `.agents/skills` root,
the roots plugins registered and the project root. It gains one thing: a *Skill installation* —
a record of a source the user chose, the Skills taken from it, and the files it owns under the
user root.

- **Preview** reads a local directory or a Git repository and returns the Skills it contains,
  without writing anything.
- **Install** copies only the selected Skills into the user skills root, records the source and
  publishes the result to the catalog.
- **Update** re-acquires the recorded source with the recorded selection.
- **Remove** deletes exactly what the record owns.

There is no manifest requirement, no second store and no second plugin. The record is a field of
the Skills business document (`skills.json`), the files live in the user root the catalog already
scans, and publication is the catalog's existing version advance.

Non-goals:

| Not done here | Why |
| --- | --- |
| Project-scope installation | Project Skills come from the project source tree; the project content it carries starts disabled and only the local user configuration can enable it ([ADR 003](../../adr/003-approval-scope.md)). The user scope is the only destination. |
| Executable plugin code, MCP configuration, hooks | They belong to plugin packages (PluginManager) and are enabled and removed there. A source that also contains code installs only its Skills. |
| A Skill editor | The browser never edits Skill files ([web workspace §4.3](../web-workspace.md#43-settings-and-account-management-on-the-local-web-entry)). |
| A market or a source index | Later (plugin management requirements). |
| A filesystem watcher | Refresh stays an explicit operation, as it is today. |
| Per-Skill installations of one source | One installation per source and installation id; its selection is recorded once. Removing one Skill of the selection is a reinstall with the smaller selection. |
| Server-side preview state | The preview is stateless: the caller keeps the preview and passes its revision back. |

## 2. Own contract

### 2.1 Source and revision

A source is a local directory or a Git repository, acquired exactly as PluginManager acquires a
plugin directory: `https://` or `ssh://` only, no submodules, no Git hooks, no terminal prompt, no
local or `ext` transport, and a tree in which no symbolic link leads outside it ([PluginManager
§7](PluginManager.md#7-security)). A local source must be an absolute path
that resolves to a directory and passes the same link check.

Each operation owns one directory, `<home>/.agents/.staging/<random>`, outside every scanned root
and on the same file system as the user skills root: the acquired tree under `src`, the
installation being prepared under `place`. The whole directory is deleted when the operation
settles, however it ends. Apart from that directory, an installation creates and deletes nothing
outside the user skills root.

Every reply and error is a safe projection: a Git URL is reported without user information, query
or fragment, and Git's own error text is never forwarded. The record keeps the URL as given, like
`plugins.json` does.

The **revision** is SkillsCatalog's content identity of a source tree, not a Git commit: a digest
over the Skills the scan finds, in scan order, of each Skill's source-relative directory and its
`SKILL.md` bytes. The same tree therefore has the same revision whether it came from a path or a
clone, and an unchanged branch is not stale. Files bundled next to a `SKILL.md` are copied but are
not part of the revision: the revision states that the reviewed Skills and their documents are
unchanged.

### 2.2 Reads

`skills.preview` takes a source and answers what installing it could add. It writes nothing.

```json
{
  "source": {"git": "https://example.com/acme/skills", "ref": "main", "commit": "1a2b3c4d"},
  "revision": "sha256:…",
  "skills": [
    {"path": "writing/review", "name": "review", "description": "Reviews a change against its spec."}
  ],
  "conflicts": [],
  "shadows": [
    {"path": "tooling/lint", "name": "lint", "by": "plugin:kit:tooling/lint"}
  ],
  "issues": [{"path": "broken/SKILL.md", "message": "missing description"}]
}
```

- `skills` lists every Skill the scan found, in scan order, by its source-relative directory
  (`path`), the `name` and `description` its frontmatter declares. `path` is the selection
  identity: a name may repeat across sources, a path cannot within one tree.
- `source` echoes what was read, projected like every other reply: a canonical path, or the Git
  URL with the commit that was acquired, so the caller can show what the selection belongs to.
- `conflicts` are reasons the install would be refused with this selection: a Skill whose name is
  already provided by another Skill in the *user* root, where the winner would depend on
  traversal order rather than on a defined precedence.
- `shadows` is information: the user root precedes plugin and project roots, so installing the
  Skill hides a Skill of that name from those roots. The catalog reports it after the fact as
  `shadowedBy`; before the install the user decides.
- `issues` are the scan's per-file problems, in the shape `skills.catalog` already returns. A tree
  with issues can still be installed; the selected paths must exist in `skills`.
- There is no pagination and no cursor: the list is bounded by the scan limits the catalog already
  applies (1000 `SKILL.md` files, depth 10, 4000-character metadata budget applies to model lists
  only), and the caller filters the list it holds. A source that exceeds the limit reports
  `ScanLimitReached` and the Skills found so far.

`skills.installations` reads what was installed and from where:

```json
{
  "installations": [
    {
      "id": "acme-skills",
      "source": {"git": "https://example.com/acme/skills", "ref": "main", "commit": "1a2b3c4d"},
      "revision": "sha256:…",
      "skills": [{"path": "writing/review", "id": "user:acme-skills/writing/review"}]
    }
  ]
}
```

The read carries no `name`, `enabled` or `shadowedBy`: those are catalog facts, and the caller
joins by Skill id. A recorded Skill that the catalog does not list is simply absent from the
catalog until the installation is updated or removed.

### 2.3 Install

```json
{
  "source": {"path": "/src/acme-skills"},
  "revision": "sha256:…",
  "id": "acme-skills",
  "skills": ["writing/review"],
  "overwrite": false
}
```

Order:

1. Validate the request shape at the boundary: source shape, `id` (`[a-z0-9_-]{1,64}`), a
   non-empty selection of distinct paths.
2. Acquire the source and compute its revision. A revision that differs from the request's is
   `skills_preview_stale`; the reply names the revision found, and nothing else happens.
3. Scan the acquired tree, and check the selection against it: every path is one the scan found
   and directly contains `SKILL.md`; no two selected Skills declare the same name; no user-root
   Skill outside this installation provides one of those names; no selected Skill contains a
   symbolic link.
4. Check the destination `<user skills root>/<id>`. It is created only by an installation: an
   existing directory or record needs `overwrite`, and nothing outside `<user skills root>/<id>`
   is ever written or deleted.
5. Copy the selected Skill directories, keeping their source-relative paths, into `place`; rename
   the existing `<user skills root>/<id>`, when there is one, to `old`; rename `place` to
   `<user skills root>/<id>`. A replacing install keeps its id and replaces the selection as a
   whole; the previous directory is deleted with the operation directory.
6. Commit the record through the Skills storage port, which writes the Skills business document
   with revision-conflict retry.
7. Rescan the user root, advance the catalog version and announce it, so a Session sees the Skills
   before the reply.

A failure in 2–5 leaves the previous installation and the record untouched. A failure in 6 removes
the new files and restores the previous directory.

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
update. The installation's revision and Git commit advance; the id and selection do not. Update is
also the repair for a recorded installation whose directory was removed by hand.

### 2.5 Remove

`skills.remove` takes an installation id, deletes `<user skills root>/<id>` after the same shape
check the owner applies to its own directories (exactly that path, real directories, no symbolic
link component), removes the record, rescans the user root and advances the version. A recorded
installation whose directory is already gone removes its record only. Unrelated Skills, other
installations and plugin-owned Skills are untouched.

If deleting the files fails, the record is kept and the reply says the files remain: the record is
the only proof of what the installation owns, so it cannot be dropped first. The user retries; the
next update reinstalls the files.

### 2.6 Ownership boundary with PluginManager

A Skill installation is not a plugin: it has no manifest, no loader row, no enablement state of
its own, and no `plugin.*` operation accepts it. `skills.install`, `skills.update` and
`skills.remove` accept only ids the Skills record holds; a plugin id is `skills_install_unknown`.
Plugin packages that carry Skills stay visible in the same settings page and keep their lifecycle
(`plugin.enable`, `plugin.disable`, `plugin.update`, `plugin.remove`), and their Skills keep the
`plugin:<plugin id>:` ids and the `pluginSkills` roots SkillsCatalog already holds.

### 2.7 Errors

| Code | When | Recovery |
| --- | --- | --- |
| `skills_source_invalid` | the source is not an absolute directory, is not an `https://`/`ssh://` URL, or a link in the tree leaves it | correct the source |
| `skills_acquire_failed` | the clone or the staging directory failed | retry; check access to the repository |
| `skills_preview_stale` | the source's revision at install differs from the request's | preview again and reselect |
| `skills_selection_invalid` | empty or repeated selection, a path the scan did not find, a repeated name in the selection, or a link inside a selected Skill | correct the selection |
| `skills_install_exists` | the id is recorded or its directory exists and `overwrite` is false | confirm overwrite or choose another id |
| `skills_install_unknown` | update or remove of an id the record does not hold | refresh `skills.installations` |
| `skills_install_conflict` | a selected name is already provided by a user-root Skill outside this installation | remove or rename the other Skill, or drop the selection |
| `skills_install_failed` | copying, swapping or recording failed; the previous state was restored | retry |
| `skills_remove_failed` | deleting the recorded files failed; the record was kept | retry; fix permissions |
| `skill_unknown`, `skills_conflict`, `skills_write_failed`, `skills_sync_failed`, `skills_unavailable` | unchanged, from the existing catalog and enablement contract | unchanged |

## 3. Events and state

The installation record is part of the Skills business document in the user scope: one map from
installation id to its source, revision and selected paths. It is written only through the Skills
storage port (`SkillSettings`), the same path that already carries the disabled ids and the plugin
roots, with the same revision-conflict retry. Fields this version does not know are preserved, so
a newer document read by an older process is not lost, and no format version is introduced: the
document has always tolerated unknown members and the new field is additive.

The record is business memory that survives a reload; nothing else is stored. The preview is not
kept, not on the host and not in the browser's persistence.

No Timeline event and no new bus event: publication is `skills.catalog_changed {version}`, which
SkillsCatalog already emits when its version advances, and the Session and branch views react to
it as they do today. The version advances only after the record is committed, so a listener never
observes files the record does not own.

`skills.json` after one git installation and one hand-written Skill:

```json
{
  "disabledSkillIds": ["user:archive"],
  "installations": {
    "acme-skills": {
      "source": {"git": "https://example.com/acme/skills", "ref": "main", "commit": "1a2b3c4d"},
      "revision": "sha256:…",
      "skills": ["writing/review", "writing/edit"]
    }
  }
}
```

## 4. Dependencies

| Service | Purpose |
| --- | --- |
| `P:HostPlatform` | The user skills root and the home the staging directory hangs under. |
| `P:SkillSettingsHost` | Read and write the Skills document: disabled ids, plugin roots and installations. |
| `P:ManagementMethods` | Register the five methods under the `skills` namespace, which the App Server's SkillsCatalog plugin already owns. |
| `P:PartRegistry` | Unchanged; the Session and branch parts these methods do not touch. |

The App Server's SkillsCatalog plugin keeps the wire boundary: it acquires a source, projects
replies, and calls the owner. The catalog keeps everything after acquisition: validation, files,
record and publication. Route management methods behind the existing local-transport admission;
paired and legacy clients see `local_transport_required` exactly as for `skills.catalog`.

## 5. Cross-module requirements

**R1. Web workspace: admit the five methods and render them ([§4.3 and §8](../web-workspace.md#8-requirements-raised-to-mechanism-owners)).**

| Field | Content |
| --- | --- |
| Problem | The Web Settings allowlist carries `skills.catalog`, `skills.set_enabled` and `skills.refresh` only, so a manifest-free source cannot be previewed, selectively installed, updated or removed from the browser. The current page offers `plugin.install` instead, which needs `dim-plugin.json`. |
| Why existing mechanisms do not suffice | `plugin.install` installs a whole package and requires a manifest; the generic `settings.update` path is user-only for the `model-log` and `execution` sources and must not become a JSON editor; a browser-side filesystem or Git write would duplicate an owner. |
| Required capability and ordering | Admission of `skills.preview`, `skills.install`, `skills.update`, `skills.remove` and `skills.installations` on the authenticated local Web entry, filtered by the same list that dispatch uses; the Skills page renders preview, selection, overwrite confirmation, update and confirmed removal from these replies, keeps the draft on failure and refreshes the catalog after success. The Web advertisement must not offer them when the server lacks them. |
| Owner and issue | Web workspace ([WebEntry](../web-workspace.md)); #740. |

**R2. PluginManager: state that source acquisition is shared ([§7 Security](PluginManager.md#7-security)).**

| Field | Content |
| --- | --- |
| Problem | §7 states the acquisition rules (allowed schemes, hardened Git, the link check, the staging directory) as PluginManager's own. Skills needs the same rules for a non-plugin source, and a second statement of them would be a second rule. |
| Why existing mechanisms do not suffice | The rules exist and are implemented; only their scope in the document is narrower than their use. Duplicating them in the Skills design would let the two drift. |
| Required capability and ordering | §7 describes acquiring an outside directory as the process-wide mechanism behind both plugin installation and Skill installation, each consumer owning its destination and its record. The Skills design then links to it and adds no acquisition rule of its own. |
| Owner and issue | PluginManager ([PluginManager.md](PluginManager.md)); #740. |

**R3. Settings: none.** The installations field is Skills' own document content and needs no new
Settings capability; the source stays registered by the same plugin that registers it today.

## 6. Rule check

| Rule | How this design complies |
| --- | --- |
| §1 Capability and App Server boundary; ADR 002 D1 | The owner of the state is the process: the record is Skills' business document, the files are in Skills' user root, and the catalog version is the publication. No new process, Session or branch part, no wiring module, and the App Server keeps only acquisition and the wire boundary. |
| §4.1 split by state owner | Discovery, enablement and installations are one state owner and one part; PluginManager keeps plugin rows and plugin content, and neither part reads the other's state. |
| §4.2 commands, not direct control | Skills is asked for an operation by a typed method call; it writes its own record and advances its own version. Nothing else writes `skills.json`, and the browser writes nothing. |
| §4.3 handlers register work and return | The methods are ordinary provider calls with `Interruption::Report`; acquisition and copying run inside the call, a cancelled call settles before the reply, and no handler writes a Timeline event. |
| §4.4 state and rebuild | The record is business memory read back from Settings after a reload; filesystem state is the installation's own I/O resource and is settled to a terminal state (present, or recorded and repaired by update). Producible facts stay out of the Timeline because nothing outside Skills decides on them. |
| §4.7 public events and services | The only observable signal is the existing `skills.catalog_changed {version}`; no new service, event or port. |
| §4.8 requirements raised | R1 and R2 above, in the §4 rule 8 format. |
| §4.9 ordering | The record commits before the version advances; a removal deletes files before the record, because the record is what proves ownership. |
| ADR 002 D4 configuration | No structural configuration: no loader row, no enablement flag of its own. `skills.json` stays business configuration. |
| ADR 002 D5 language-neutral contract | §2 is stated as source, revision, selection, id and outcomes in neutral terms; the Rust and JSON spellings are in the appendix. |
| ADR 003 / file-root authorization | Installation grants no authority: an installed Skill is an instruction file the model loads like any other, and a source is read only where the operation names it. |

Ablation: no preview cache, no preview registry, no second root kind, no second store, no second
plugin, no per-Skill record, no new event, no pagination cursor, no progress declaration (the Web
renders a busy state; acquisition phases have no consumer today). The preview is stateless and the
install re-acquires, so the only added state is the record plus one staging directory that is
removed when the operation settles.

## 7. Binding appendix

Method declarations, in the App Server's `SkillsCatalog` plugin (Rust), `mutating` and
`Interruption::Report` as shown; `skills.preview` and `skills.installations` are not mutating:

```text
skills.preview        read
skills.install        mutating, no progress
skills.update         mutating, no progress
skills.remove         mutating, no progress
skills.installations  read
```

Owner-side additions (Rust, `dim-agent-capabilities`):

```rust
pub struct SkillInstallation {
    pub source: SkillInstallSource,
    /// SkillsCatalog's revision of the selected Skills.
    pub revision: String,
    /// Source-relative Skill directories, in install order.
    pub skills: Vec<String>,
}

pub enum SkillInstallSource {
    Path { path: PathBuf },
    /// `reference` is serialized as `ref`, as in `plugins.json`.
    Git { git: String, reference: Option<String>, commit: String },
}

// SkillSettings gains, next to set_plugin:
fn set_installation(
    self: Arc<Self>, id: String, installation: Option<SkillInstallation>,
    cancel: CancellationToken,
) -> BoxFuture<'static, Result<SettingsVersion, SkillSettingsError>>;
```

`SkillSettingsSnapshot` gains `installations: BTreeMap<String, SkillInstallation>`, and
`SkillDocument` in the App Server serializes it under `installations` with unknown members
preserved. New `SkillsError` variants: `InstallationExists`, `UnknownInstallation`,
`StalePreview { expected, found }`, `SelectionInvalid(String)`, `InstallConflict(String)`,
`InstallIo(String)`; the App Server maps them to the codes in §2.7. `SourceInvalid` and
`AcquireFailed` stay on the App Server side, where acquisition happens.

## 8. Acceptance

Owner flows, in [skills.rs](../../../tests/integration/tests/skills.rs), over a disposable home
and a source directory with several Skills and no manifest:

- preview lists every Skill with its source path, description and issues, and writes nothing;
- install copies only the selected Skill directories, leaves other files in the home and the
  project untouched, and the catalog lists them under `user:<id>/…` before the reply;
- a Skill changed between preview and install refuses with `skills_preview_stale` and writes
  nothing;
- a name already provided in the user root, a repeated name in the selection, a `..` or absolute
  selection path, and a source whose link leaves it are each refused, with nothing written;
- install over an existing installation needs `overwrite`, and with it replaces the selection
  without touching another installation or a hand-written Skill;
- update keeps the selection, refuses a selected path that disappeared, and repairs a directory
  removed by hand;
- remove deletes only `<user skills root>/<id>` and its record, and removes a record whose
  directory is already gone;
- the record survives a catalog reload and a process restart.

App Server flows, in [transport_flow](../../../crates/dim-app-server/tests/transport_flow/), over
a disposable home and a local Git fixture: preview, install, catalog, update to a new commit and
remove through the real protocol; the Web entry advertises the five methods while a remote and a
legacy connection refuse them; a credential-bearing URL and a failing clone produce replies and
errors without the URL or Git's stderr; `skills.json` keeps unrelated fields and a document
written before this field existed still loads.

Rendered verification follows the Web workspace's acceptance for the Skills page: desktop and
compact layouts, preview with conflicts and shadows, cancelled and accepted overwrite, a failed
acquisition with retry, stale preview, update and confirmed removal, and a draft kept on failure.
