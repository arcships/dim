# Skills

The Skills plugin is SkillsCatalog at the process. It registers Skills in every Session and SkillsBranch in every branch; each enabled Skill is a SkillContent row of its Session's SkillContentInstances group.

## SkillsCatalog

Owner: [skills/catalog.rs](../../../../crates/dim-agent-capabilities/src/skills/catalog.rs)
and [discovery.rs](../../../../crates/dim-agent-capabilities/src/skills/discovery.rs).
The Skills plugin. It owns process-wide skill discovery caches and enablement,
combining the user `.agents/skills` root, the skills roots plugins registered
and the project `.agents/skills` root, in that order of precedence, into
queryable views with issues, and registers the Skills parts that read them:
[Skills](Skills.md#skills) in every Session and [SkillsBranch](Skills.md#skillsbranch)
in every branch. The App Server's
plugin of the same name mounts this catalog, adds its management methods and
registers the same parts.

### Dependencies

| Service | Purpose |
| --- | --- |
| `P:HostPlatform` | Resolve the user home root. |
| `P:SkillSettingsHost` | Read and update enablement. |
| `P:PartRegistry` | Register the Skills parts, until this plugin unloads. |

### Events and lifetime

No Timeline events. Each time its version advances (a rescan, or a changed
disabled set) the catalog emits `skills.catalog_changed {version}` on
`catalog_changes()` (`skills/catalog`) from its own context, under its state
lock, so announcements follow version order; listeners query the catalog for
the view.

Before its first settings read, the catalog listens with `ctx.events().on` to
`settings.changed` on `settings_changes(SOURCE)`, its Settings source `skills`.
The listener only wakes a tracked reader, which rereads the settings one read
at a time so a stale response cannot overwrite newer settings. A host whose
storage changes outside the catalog announces those writes there; the catalog
reads back its own writes, so `MemorySkillSettings` (xtask, tests) announces
nothing.

A plugin's skills root is registered with `register_plugin(plugin, root)` and
withdrawn with `withdraw_plugin(plugin)` ([PluginManager §3.7](../../../design/plugins/PluginManager.md#37-content-registration)).
The catalog keeps the roots in its settings (`pluginSkills`), so they come back
after its reload; registering scans the root at once and returns its issues, and
`plugin_issues(plugin)` reads them later. Its Skills have ids
`plugin:<plugin id>:<path>`.

### Installations

A Skill installation records a source the user chose, the Skills taken from it and the files it
owns under the user root ([design](../../../design/plugins/Skills.md)). It is a member of the same
Skills document (`installations`), read and written through the same port; its files live under
`<user root>/<installation id>/<source-relative path>`.

| Operation | Behavior |
| --- | --- |
| `preview(root, id)` | Scans an acquired source tree, digests each Skill directory, and reports the user-root Skills a selection would conflict with and the plugin or project Skills it would hide. Writes nothing. |
| `install(id, source, root, operation_dir, selection, overwrite)` | Digests the selected Skills and refuses a changed one (`StalePreview`), copies them into `<operation_dir>/place`, swaps them into place, commits the record, then rescans the user root and advances the version. |
| `uninstall(id)` | Deletes `<user root>/<id>` after a directory and link check, then the record, then rescans. A failed deletion keeps the record, the only proof of what the installation owns. |
| `installation(id)`, `installations()` | The record, after a query. |

A Skill's revision is a digest over its regular files, by relative path and bytes, with `.git`
skipped and symbolic links refused; an installation's revision digests its ordered path and
revision pairs. A separate `operating` lock serializes every root scan with an installation's
placement, so no view is built from a half-swapped directory. Files are written before the record
and the record before the version advances.

First use scans roots via blocking work; explicit refresh updates cached
discovery. Cancellation closes the catalog; cleanup joins its reader. Reload
creates fresh discovery caches. Read failures remain explicit and queries can
retry unavailable settings; this is not a filesystem watcher or proof that a
settings mutation was durably saved.

### Tests

[skills.rs](../../../../tests/integration/tests/skills.rs):
`discovery_precedence_and_list_injection`,
`enablement_changes_the_list_and_loading`,
`a_late_update_response_does_not_overwrite_a_newer_setting`,
`a_failed_read_back_is_reported_and_recovered_by_the_next_query`,
`preview_lists_a_source_without_installing_it`,
`install_copies_only_the_selected_skill`,
`a_change_under_a_selected_skill_makes_the_preview_stale`,
`install_refuses_conflicts_and_selections_outside_the_tree`,
`install_replaces_an_installation_only_with_overwrite`,
`update_keeps_the_selection_and_reports_the_previous_record`,
`remove_deletes_only_what_the_record_owns`,
`a_new_catalog_restores_installations_from_settings`;
[plugin_manager.rs](../../../../crates/dim-app-server/tests/transport_flow/plugin_manager.rs):
`plugin_content_reaches_sessions_through_its_owners`;
[skills_installation.rs](../../../../crates/dim-app-server/tests/transport_flow/skills_installation.rs):
`skills_preview_install_update_and_remove_a_repository`,
`skills_replies_and_the_user_document_stay_safe`.

<!-- generated:plugin SkillsCatalog — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 进程 |
| 注入 | `P:HostPlatform`、`P:PartRegistry`、`P:SkillSettingsHost` |
| 提供 | `P:SkillsCatalog` |
| 被注入于 | `Skills` |
<!-- /generated -->

## Skills

Owner: [skills/session.rs](../../../../crates/dim-agent-capabilities/src/skills/session.rs).
Selects content from the catalog, owns Session loader content instances and
aggregates their contributions into the skill tool and instructions/list prompt sources.

### Model-facing texts

Skills writes two texts for the model. Each is protocol or policy as
[model-facing prompts §4](../../../design/model-prompts.md#4-protocol-and-policy) decides;
the instructions are mixed.

| Text | Class and sentences |
| --- | --- |
| `dim/skills/instructions`, in the system prefix | Protocol: "A skill is a local instruction bundle stored in a SKILL.md file." states what a skill is, "Discovery: The latest `<dim-state source="dim/skills/list">` lists the skills available in this session, each with a name and, when space allows, a description." names the state source the list arrives in, "A `$skill: name` marker names the exact skill to load." names a marker, and "Use `skill({ "skill_name": "<name>" })` to load the full SKILL.md instructions." states the call shape. Policy: the trigger rules, loading, multiple-match, missing-or-blocked, context-hygiene and safety guidance, and "Use the `skill` tool to load the full instructions before applying a skill." |
| `dim/skills/list`, as a `<dim-state>` block | Protocol: the format — one `- <name>: <description>` or `- <name>` line per enabled skill, names listed always and descriptions added cheapest first within the character budget, escaped — and the source name the block carries. The names and descriptions are the skills' own data |

The list arrives in a `<dim-state>` block, so its guide entry is Prompt's
([Prompt §4](../../../design/plugins/Prompt.md#4-the-built-in-entries-and-their-writers)):
Skills writes no marker of its own and contributes no entry. The `skill` tool's
declaration is built-in and mixed: its schema is protocol, and so is "Load and
apply a named skill's full SKILL.md instructions." because it states what the
tool loads; the rest is policy — the `BLOCKING REQUIREMENT` rule, what makes a
request match a skill, and invoking the tool before answering. Those policy
sentences have no factory member yet, and get one in the change that first
evaluates a candidate for them. Its result is
Skills' own: the `Skill:`, `Description:`, `File:`, `Allowed tools:`
and `Instructions:` layout and the refusals "Invalid input: skill_name must be a
string", "Skills unavailable: …", "Skill is disabled: …" and "Unknown skill: …" are
protocol, and the loaded body is the skill author's own text.

### Dependencies

| Service | Purpose |
| --- | --- |
| `P:SkillsCatalog` | Query user/project skills; its `skills.catalog_changed` events drive replacement. |
| `S:SessionConfig` | Select the Session working directory. |
| `S:SessionLoader` | Lock-free creation of Session-owned content instances. |
| `S:SessionLifecycle` | Start catalog-driven replacement only after the Session is published. |
| `S:SkillsConfig` | The factory configuration of this plugin, registered and provided by `skills::install_with_config`; its `instructions` text replaces the built-in skill instructions when set, and a profile that registers the parts without the install declares and reads none. |
| `S:PromptRegistry` | Register the list and instructions sources, until this part unloads. |
| `S:ToolRegistry` | Register the `skill` tool, until this part unloads. |

### Content instances

`SkillContentInstances` is an instanced Session extension template. Each enabled,
active catalog definition creates a `SkillContent` leaf with an immutable private
definition and a unique scoped contribution key passed through
`create_instance().with(...)`. Definitions and bodies are not loader row options.
The leaf provides its content service and adds the same service to the
collection of ready content that Skills holds for its generation and hands to
each leaf in its instance value; disposal withdraws it. The collection is no
service, so a leaf does not wait for Skills, which waits for its leaves: no
apply-time dependency cycle and no undeclared dynamic read. Each Skills
generation closes its leaves with itself and starts a new collection.

The catalog still owns source precedence, shadowing and enablement. Skills owns
selection and reconciliation, not a second lifecycle tree. Unchanged definitions
retain their instances. Body-only refresh replaces only changed content and changes
the view digest, without repeating unchanged list text. Views are built from ready
content services, never directly from enabled catalog definitions. A view is
rebuilt at each catalog change; a leaf withdrawn on its own does not change the
current view until then.

Apply listens to `catalog_changes()` with `ctx.events().on` before its first
query and creates the initial instances as part of Session assembly. The
listener only wakes the Session's tracked replacement task: rutis awaits one
channel's listeners in turn, and every Session listens on the same channel.
Replacement after a catalog change waits for `SessionLifecycle::activated()`,
so assembly readiness never observes an instance that the replacement task
retires, and a Session that fails assembly never replaces content. Changes
announced meanwhile are adopted by the first update after publication. Replacement ends when
`SessionLifecycle::closing()` returns: closing wins over a simultaneous change,
and an update in progress is cancelled but awaited, so a closing Session creates
no content and abandons no partial instance. Cancellation is not logged as a
content failure.

### Events and lifetime

No direct Timeline events. The tracked replacement task replaces the Session
view; SkillsBranch requests list adoption. The tool returns full skill content as a
plain tool result, not a second history-rendering route. Cleanup joins the
replacement task and shuts down every content fiber; tool handlers reject work after
generation cancellation. Initial creation also runs in a tracked task, so dropping
apply cannot abandon it. Creation is awaited to settlement even if cancelled,
then an unusable instance is shut down. Failed updates drain partial/current
instances and publish an unavailable view. `FiberView::shutdown` and its retained
error preserve cleanup failures; no lock-taking loader mutation runs in apply or
dispose. Content lifecycle failures use the existing public `SkillsError::Closed`
state, with details logged; disposer failures propagate to Session shutdown.
Catalog errors retain their existing public variants. Closing one Session cannot
dispose another Session's contents. Reload queries
a fresh view. Unavailable settings isolate skills without disabling unrelated
execution. Listed metadata and loaded body can change between view updates;
loading uses the current view rather than an old model-visible list.

### Tests

[skills.rs](../../../../tests/integration/tests/skills.rs):
`load_returns_the_body_as_a_plain_tool_result`,
`enablement_changes_the_list_and_loading`,
`skill_loads_the_latest_view_after_queue_and_steer_interleave`,
`content_instances_are_session_owned_and_only_changed_content_is_replaced`,
`catalog_changes_during_assembly_wait_for_session_publication`,
`a_closing_session_creates_no_content_instances`,
`unavailable_settings_isolate_skills_and_refresh_recovers_existing_sessions`.

<!-- generated:plugin Skills — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `P:SkillsCatalog`、`S:PromptRegistry`、`S:SessionConfig`、`S:SessionLifecycle`、`S:SessionLoader`、`S:SkillsConfig`、`S:ToolRegistry` |
| 提供 | `S:SessionSkills` |
| 被注入于 | `SkillsBranch` |
<!-- /generated -->

## SkillsBranch

Owner: [skills/session.rs](../../../../crates/dim-agent-capabilities/src/skills/session.rs).
Exposes the Session skill view from a branch and requests adoption of a changed
skill list through `ConfigureControl`; initial adoption belongs to Prompt.

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:SessionSkills` | Current skill view and change watch. |
| `B:BranchLifecycle` | Wait for activation; stop on closing. |
| `B:dyn ConfigureControl` | Adopt the changed list as prompt configuration. |

### Events and lifetime

No direct Timeline subscription or production. A tracked watcher starts after
activation, coalesces Session view changes and requests `dim/skills/list`
configuration. Generation cancellation stops the watcher; cleanup joins it.
Reload creates a new watcher, not a filesystem watcher or durable catalog.
Failed adoption is logged; a later view change can retry.

### Tests

[skills.rs](../../../../tests/integration/tests/skills.rs), especially
`a_view_change_inside_a_turn_reaches_the_next_request` and
`enablement_changes_the_list_and_loading`.

<!-- generated:plugin SkillsBranch — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:BranchLifecycle`、`B:dyn ConfigureControl`、`S:SessionSkills` |
| 提供 | `B:SkillsBranch` |
| 被注入于 | — |
<!-- /generated -->

## SkillContentInstances

Owner: [skills/mod.rs](../../../../crates/dim-agent-capabilities/src/skills/mod.rs).
The instanced loader group containing one [SkillContent](Skills.md#skillcontent) leaf.

### Dependencies

None on the group itself. The leaf declares its Session contribution dependency.

### Events and lifetime

This is a structural template, not an installer or a second lifecycle manager.
Skills creates instances in the Session subtree and supplies immutable private
content. Closing the Session disposes its groups. Template configuration never
carries mutable per-Session definitions. See [Skills](Skills.md) for selection.

### Tests

[skills.rs](../../../../tests/integration/tests/skills.rs) covers independent
instances and selective replacement through public Session flows.

<!-- generated:plugin SkillContentInstances — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | — |
| 提供 | — |
| 被注入于 | — |
<!-- /generated -->

## SkillContent

Owner: [skills/content.rs](../../../../crates/dim-agent-capabilities/src/skills/content.rs).
An immutable enabled Skill definition contributed by a Session-owned loader instance.

### Dependencies

None: its instance value carries what it needs.

### Events and lifetime

No Timeline events or external execution. Private instance values contain the
definition, the scoped service key and the Skills generation's collection of
ready content; loader options never contain the Skill body. The leaf provides
its definition and adds that same service to the collection. Its effect withdraws the contribution. Skills coordinates selection;
rutis owns the instance lifecycle. See [Skills](Skills.md) for update and failure rules.

### Tests

[skills.rs](../../../../tests/integration/tests/skills.rs) covers loading actual
content, selective replacement, disabled content and independent Session closure.

<!-- generated:plugin SkillContent — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | — |
| 提供 | `S:Contribution#user:graph` |
| 被注入于 | — |
<!-- /generated -->
