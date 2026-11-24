# PromptGuide

Scope: session. Issue: #2026.

PromptGuide owns the Session's input guide: the frame, the entries the parts register for the markers they write, and the composition that turns the entries in force for one branch into the System a request carries. The current implementation is described in the [Prompt plugin page](../../architecture/rust-agent/plugins/Prompt.md#promptguide); the branch's adoption of the composed System is Prompt adoption's ([#2027](https://github.com/arcships/dim/issues/2027)).

## Use and scope: responsibility split

PromptGuide owns:

- **The frame.** The head and tail of the guide, including the channel rule, which is protocol ([The frame](#the-frame)).
- **Registrations.** One entry per registration under its scope and first marker; a row may register several, and each ends when its row unloads.
- **Uniqueness per composition.** A marker has at most one entry in force for a branch; a session-scoped entry meets every branch, and two branch-scoped entries meet when they name the same branch.
- **The two entries for the markers Prompt writes**, `dim-state` at order 60 and `dim-rule` at 70.
- **The composition.** Entries ordered by their own order and first marker, joined with the frame into one System ([The composition](#the-composition)).

Callers keep:

- The text they write and the markers they emit; the entry explains the marker and is the writer's.
- When to compose and where the System goes: the branch's adoption and the request assembly ([#2027](https://github.com/arcships/dim/issues/2027)).
- Which branch a request belongs to: the caller passes the branch id.

Not this owner's:

- An entry's words other than the two shared entries: the part that writes the marker, stated on that part's page.
- The request shape: Loop; the adopted configuration: Prompt adoption ([#2027](https://github.com/arcships/dim/issues/2027)); the summary path: ContextPolicy.
- The state and rule sources and the words the model reads in them: their source owners.
- Each capability's list of model-facing texts with its protocol and policy marks, and the flow assertion in that writer's own flow ([#551](../model-prompts.md#92-every-capability-that-writes-model-facing-text), [#552](../model-prompts.md#94-timelinetree), [#553](../model-prompts.md#95-goal), [#554](../model-prompts.md#96-contextcompaction)).
- Removing the two placements: [#555](../model-prompts.md#97-loop-and-contextpreparation).

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A part explains the markers it writes, for its row's lifetime | No: the contributor comes from the call, and the marker uniqueness and the row's lifetime are the owner's | `register` |
| A part explains a marker for one branch only | Yes for the scope, which the caller states; the owner decides the meeting rule | `register` with a branch scope |
| A part places its entry among the others | Yes: the entry's `order` is the writer's data, and the owner only sorts by it | Business; the entry's `order` travels with `register` |
| A part composes the guide for one branch over a base | No: the entries in force and the frame are the owner's state | `compose` |
| Prompt re-composes when an entry appears or withdraws | No: a poll loop in the caller misses withdrawals and order changes | `observe`, then `compose` |
| Change an entry's text | Yes: register the new entry from the writer's next load | Business |
| Decide where the System goes | Yes: the branch's adoption and the request assembly place it ([#2027](https://github.com/arcships/dim/issues/2027)) | Business |

An operation exists only where the row says No; the entry's `order` is data that travels with `register`.

## Caller example

A Session part registers the entry for the markers it writes, from its `apply`:

```rust
guide.register(
    ctx,
    GuideScope::Session,
    GuideEntry {
        markers: vec!["dim-event".into(), "dim-event-guide".into()],
        order: 130,
        text: EVENT_ENTRY.into(),
    },
)?;
```

A branch part registers for its own branch:

```rust
guide.register(
    ctx,
    GuideScope::Branch(branch.clone()),
    GuideEntry { markers: vec!["dim-branch-lines".into()], order: 40, text: ROSTER_ENTRY.into() },
)?;
```

`register` answers `invalid_entry` (fix the marker or the scope), `no_caller_row` (produced by the Session dispatch only; register from a part's `apply`), `duplicate_marker` (change the marker or the scope), `too_large` (shorten the text) and `closed` (the framework refused the effect at the calling row's subtree or root close; register again from the next load).

The branch's adopter composes over the text the branch already carries, and observes the composition:

```rust
let system = guide.compose(branch.clone(), Some(SystemPrompt::Text(source)));
guide.observe(ctx, move |version| {
    let _ = sender.try_send(version);
}).await?;
```

`compose` has no failure of its own, and `observe` answers `closed`; a composition already made is not changed by a later withdrawal.

## Own contract

### Dependencies

None.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `register` | Publish an entry for the calling row's lifetime | `{scope, entry}` | no value | None; one atomic step under the table's lock | `invalid_entry`, `no_caller_row`, `duplicate_marker`, `too_large`, `closed` |
| `compose` | The System for one branch over a base | `{branch, base?}` | the System (`SystemPrompt`) | None; a pure read | None |
| `observe` | The table revision now and after each change until the row unloads; crate-internal | no data; a listener callback by reference | no value | The row's unload ends the subscription | `closed` |

`register` and `compose` are the public data face and both are inherent methods on the owner type; `compose` is also the Rust binding the flow tests read, and `observe` is `pub(crate)` to `dim-agent-capabilities` because only Prompt subscribes.

`scope` is `"session"` or `{branch: <branch id>}`; `entry` is `{markers, order, text}`, `markers` at least one and each `[a-z][a-z0-9_-]{0,63}`, `text` at most 4096 bytes. `listener` is a callback by reference, not a request field.

`register` checks the scope, the markers, the size and the duplicates, then records the entry under the calling row: it has either taken effect or answered an error, and a refused call registers nothing. An entry becomes visible to `compose` only after its cleanup is registered, so a composition never reads an entry the table refused; the two read and write under the table's own lock.

`compose` answers the System for the branch, composed as [The composition](#the-composition) states, over the base the caller passed. A text base precedes the guide after one blank line; an empty text base is the guide alone; a parts base gains the guide as one more text part with no cache control of its own, so a caller keeps its own caching boundaries. The branch id is a value: an empty id, or one that names no branch, composes the Session's entries, and whether a branch exists belongs to the session tree; `compose` does not repeat `register`'s scope check. A composition made after the table unloads is terminal: `compose` answers the frame and the entries still registered, no caller adopts it, and an adopter composes only while the table is loaded. The base is a typed value the caller or the dispatch decodes before the call, so a malformed base cannot reach `compose`, and `compose` answers without an error kind.

`observe` starts one coalescing delivery task for the subscription, owned by the table and cancelled at the row's unload; the table's shutdown aborts the task instead of waiting on an arbitrarily long listener, and the listener must return promptly and must not block. A panicking listener is contained and does not end the subscription. The task calls the listener with the table revision at subscription and again after later registrations and withdrawals; deliveries coalesce, so a listener that falls behind reads the latest revision rather than a queue of every change, and the table holds no lock of its own or Timeline commit lock while the listener runs. The revision belongs to one generation of the table: a reload starts a new table, a new counter and a new subscription, and revisions compare only within one generation. The first delivery is the listener's starting point, not a snapshot taken at subscription, so the listener recomposes and compares when it arrives, because an entry may register between its own composition and the subscription. The revision counts changes in the Session, not this branch's composition, so a listener recomposes for its branch and compares. The listener returns nothing: what it does with the revision, and any failure in that work, belongs to the subscribing row, and neither ends the subscription nor undoes a registration.

### Errors

| Kind | Operation | What the caller does next |
| --- | --- | --- |
| `invalid_entry` | `register` | A marker is outside the grammar, the entry has no marker, or the scope's branch id is empty; fix the entry |
| `no_caller_row` | `register` | Only the Session dispatch produces it, because a Rust caller always has its `Ctx`; the calling row could not be resolved, so register from a part's `apply` |
| `duplicate_marker` | `register` | A marker the entry names has an entry whose scope meets this one; change the marker or the scope, or ask the other entry's owner to stop registering it at its next load, because one entry explains a marker |
| `too_large` | `register` | The text is over 4096 bytes; shorten it |
| `closed` | `register`, `observe` | The framework refuses the effect because the calling row's subtree or root is closing; register or subscribe again from the next load |

The closing rule is one: `register` and `observe` are effects of the calling row, so the framework refuses them when that row's subtree or root closes; the Dim Session's own closing is not such a close, so a registration that lands while the Session is closing but whose row is still open takes effect and ends with that row. The table reads no Session phase: `closed` is the framework's refusal, not a check the owner makes. Once the table unloads no injected row calls it, and a row that observes subscribes again as it reloads.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `register` | The scope, markers, size and duplicates pass, the entry is recorded under the row, and the version moves | — | The entry is withdrawn and the version moves; every observer hears it | New registrations answer `closed` and the table ends with its Session; a reload replaces the table, the rows that depend on it reload and register again, and nothing from the old table carries over | The registering row |
| `compose` | Select the entries in force for the branch, order them and join the frame over the base | — | Nothing: a System already returned stands | Nothing: a System already returned stands, and the rows that inject the table reload against the next generation's | PromptGuide |
| `observe` | Register the listener under the row and call it with the current version | Not cancellable; the subscription ends at the row's unload | The subscription ends after the delivery in flight finishes | The table cancels the delivery task and aborts it before it unloads; a row that observes subscribes again as it reloads | The subscribing row ends the subscription; PromptGuide aborts the task at shutdown |

### The entry route

**Identity.** A registration belongs to two things: the loaded row that makes it, and the markers its entry explains. The contributor comes from the call — its `ctx` in Rust, the calling row through the bridge — never from the payload, so no caller registers in another's name.

**Scope.** The table is the Session's, because contributors live at both Session and branch scope and the Session is their nearest common owner. Each registration states where its entry applies: `session` for every branch of the Session, or one branch id for that branch alone. A scope is the caller's to state: a Session row states `session`, and a row of a branch states that branch. The table cannot check it, because the call carries the caller's `Ctx` and no row-to-branch mapping, so a mismatch is not refused. A mismatch changes what the entry means, never how long it lives: a branch scope named by a row that is not of that branch is in force on the named branch until that row unloads, so it can meet the branch's own registrars, and a `session` scope from a branch row is in force on every branch, including those where its writer is not loaded. The lifetime guarantee holds for every registration: an entry is in force exactly while its registering context is loaded. A caller that stated the wrong scope registers again with its row's scope; the table does not refuse the first registration. A Rust caller that is not a part's row, such as a test root, states the scope it means, and its entry ends with that context.

**Uniqueness is per composition.** For one branch, a marker has at most one entry in force. A session-scoped entry meets every entry for the same marker; two branch-scoped entries meet only when they name the same branch. A registration is keyed by its scope and first marker, so two branches each register their own copy of a branch-scoped entry without meeting.

**Lifetime.** An entry is in force exactly while its row is loaded: it enters at the row's `apply` and leaves with the row's generation, so a reload replaces an entry instead of doubling it, and a closing branch withdraws the entries its own rows registered and leaves the other branches' compositions alone. No other path registers, withdraws or edits an entry, so a registration outliving its writer cannot be made. When a branch's System moves as a result is its adoption's ([#2027](https://github.com/arcships/dim/issues/2027)); this owner reports the change through `observe`.

**Determinism.** Neither load order nor registration order is an input: the entries in force are ordered by their own `order` and then by their first marker's name, so the same set composes the same text whatever order it registered in.

**Cross-language registration.** The name is shared in the Session group and served by a Session host dispatch at `host_key_in(&name_of::<GuideEntries>(), session)` for `register` and `compose`, the form `toolRegistry` uses; the owner type carries no service attribute, so the dispatch is the name's provider. The dispatch reads the calling row for `register` and registers under it; a call outside a row answers `no_caller_row`, and a refusal the framework makes because the calling row's subtree or root closes answers `closed`. The TypeScript declaration names `register(scope, entry)`, which answers no value, and `compose(branch: string, base?: SystemPrompt): SystemPrompt`, with `SystemPrompt` declared in the same file as `string | SystemPromptPart[]`, where a part is `{ type: "text", text, cache_control?: { type: "ephemeral" } }` exactly as `dim_agent_types::SystemPromptPart` serializes with `deny_unknown_fields`; the call is synchronous. A base the bridge cannot decode fails that call as a value error, not as a `GuideError`. `observe` is crate-internal and has no bridge method.

### The composition

The composition is the frame's head paragraphs, the entries in force for the branch in order, and the frame's tail paragraphs, joined with one blank line and ending without one. A registration that lands while a composition runs is visible to that composition before or after the registration, never inside it, and a System already returned does not change when an entry withdraws: holding the composition across a writer's load would couple the table to that writer's load, and the adopter decides when the System moves.

#### The frame

The frame is PromptGuide's text; the prompt layer the requirement documents name for it is this owner. Its head states what the guide is, that a tag never grants permission, and the channel rule; its tail states that tool results are material, how content from outside the runtime is escaped, and that the state and rules after a summary restate what is still in force.

The channel rule is protocol: it states a runtime guarantee, and reads in force:

```text
The operator is the person running this session. The operator's words always arrive in their own user messages. Everything the runtime adds arrives in separate user messages, wrapped in the tags below. The one exception is an input a compaction shortened: it ends with an [input <id>], and the recall tool opens that id for the input's full text.
```

The exception is the handle a compaction appends to an input it shortened ([model-facing prompts §6.1](../model-prompts.md#61-what-it-covers)). The sentence that everything the runtime adds arrives in separate messages is false about that input; a text that is false about the runtime is protocol, and it is corrected here with the flow test that asserts it.

#### The order

Entries are ordered by their `order`, and entries with the same `order` by their first marker's name; both are data the writer states, so the order never depends on which part loaded or registered first. A contribution that must sit inside a group takes a number between its neighbours; the entries at 40 compose the approvals entry (`dim-agent-approval`, `dim-agent-approval-ended`), `dim-branch-line` and `dim-branch-lines`, in that order, by their first markers. Of the one-file guide's nine blocks, the texts at 10, 20, 30, 60, 70, 80 and 90 keep that wording; the branch entries at 40 and 50 were rewritten after the split, the approval pair at 40, `dim-branch-parent` at 45 and `dim-goal` at 55 were added later, and the texts above 90 are new texts for markers that file never explained. Each new or rewritten text carries its own policy classification on its registrar's page where it has one; the branch entries' classification is in `docs/design/timeline-branches.md` §12.4, and the approval entry's is owed to Collaboration (#2020).

#### One composition, two paths

Two callers compose for a branch: the branch's adoption, over the source text it adopted, and the request assembly that has no adopted configuration, over the System it carries ([#2027](https://github.com/arcships/dim/issues/2027)). Both get the same frame and the same entries in force for that branch, because both call `compose`; when each one composes and where it places the result is its caller's. No contributor appends text to a request, and no request assembles a guide of its own.

### The built-in entries and their registrars

A plugin-specific entry is registered by the part that writes its marker, from that part's `apply`: the registrar is where the marker's lifetime comes from. Three cases are exceptions, four entries: `dim-state` and `dim-rule` are written by Prompt and by runtime snapshot writers and registered by PromptGuide; `dim-digest` is written by the policy and registered by ContextPreparation's branch part, which binds the policy; and `dim-branch-parent` is written by BranchLine on one branch and registered by TimelineTree at session scope. The `Entry` column gives each entry in a phrase; the page named with each registrar states the entry where it has one, and the registrars listed after the table have none yet.

| Order | Markers | Registrar and page | Scope | Entry |
| --- | --- | --- | --- | --- |
| 10 | `dim-notification` | BackgroundExec (#2028) | branch | The exec notification: a background command's outcome. |
| 20 | `dim-agent-report` | Collaboration §2 | session | The report of delegated work. |
| 30 | `dim-agent-message` | Collaboration §2 | session | A message from another agent. |
| 40 | `dim-agent-approval`, `dim-agent-approval-ended` | Collaboration (only in `crates/dim-app-server/src/collaboration/approvals.rs:766`) | session | A pending member tool action, and its retirement. |
| 40 | `dim-branch-line` | TimelineTree (#2024) | session | Which branch of the Session this is. |
| 40 | `dim-branch-lines` | BranchLine (#2024) | branch | The roster of the lines this line opened. |
| 45 | `dim-branch-parent` | TimelineTree (#2024) | session | The parent branch's new activity. |
| 50 | `dim-branch-message`, `dim-branch-idle`, `dim-branch-ended` | BranchLine (#2024) | branch | A message from another branch, and the runtime's reports about a branch. |
| 55 | `dim-goal` | Goal A.5.1 | session | The goal state block. |
| 60 | `dim-state` | PromptGuide (this section) | session | What a state block is. |
| 70 | `dim-rule` | PromptGuide (this section) | session | What a rule block is, and how it ends. |
| 80 | `dim-summary` | SummaryStrategy's Session part; ContextCompaction §15 | session | What a summary is. |
| 90 | `dim-digest` | ContextPreparation's branch part; ContextCompaction §15 | branch | What a digest is. |
| 100 | `dim-shell-input` | ShellInput §5.2 | session | The operator's own shell command and its output. |
| 110 | `dim-scheduler` | Scheduler §5.2 | session | The delivered prompt of a scheduled job, and a wakeup's prompt. |
| 120 | `dim-workflow` | Workflow §5.2, when the package is installed | session | The run's report and its stage markers. |
| 130 | `dim-event`, `dim-event-guide` | EventHub A.6 | session | The delivered event, and the guide of the source that delivered it; a factory member for evaluation, the shipped text by default. |
| 140 | `img` | ImageUnderstanding §5 M3 | session | An image reference, and how to ask about it. |

Some entries are covered without their exact text on a `docs/design/plugins/` page: `docs/design/timeline-branches.md:482-486,560` states the branch blocks and their guide-entry description, and `Collaboration.md:20,32` the report and message markers, whose texts are in `crates/dim-app-server/src/collaboration.rs:3507,3510`. The exact text on a plugin page is still missing for BackgroundExec's `dim-notification` (#2028), TimelineTree's `dim-branch-line` and `dim-branch-parent` (#2024), and BranchLine's `dim-branch-lines`, `dim-branch-message`, `dim-branch-idle` and `dim-branch-ended` (#2024); the approval entry's text is only in `crates/dim-app-server/src/collaboration/approvals.rs:766`.

Each registrar other than PromptGuide adds one dependency, its Session's `guideEntries`, and registers from its `apply`; none needs a new service, event or configuration, because the registration is a call to the owner, the way a tool registers with `toolRegistry`.

PromptGuide registers the two entries for the markers Prompt writes: `dim-state` and `dim-rule`, both session-scoped. Prompt writes both markers for its registered sources, and runtime parts may also append unkeyed `dim-state` snapshots through the input path; both forms reuse the one entry, and no part registers a competing entry. The state entry explains the latest observed value of a source, not a request or a permission, and it promises no retention: the input key governs compaction replay ([model-facing prompts §3](../model-prompts.md#3-channels)). The rule entry explains `op`'s values and the scope a rule binds.

A compaction's replacement belongs to the policy every strategy is called through, not to a strategy: the policy composes the digest after any strategy returns ([ContextCompaction §5.10](ContextCompaction.md#510-class-3a-the-digest)). The `dim-digest` entry therefore registers with the part that binds the policy, and only while a strategy is bound; a Session with no strategy compacts nothing and writes no digest, and a Session with another strategy still sends `<dim-digest>` and its guide explains it.

A capability that writes a marker and registers no entry is a capability the guide does not explain; one that registers an entry for a marker nothing writes leaves a promise the runtime does not keep. Both are the writer's to fix, and [#551](../model-prompts.md#92-every-capability-that-writes-model-facing-text) makes each writer assert the pair in its own flow.

### The facts that reproduce the entries

PromptGuide keeps no state a restart loses. The entries in force follow from the loaded parts: the same assembly loads the same parts, each registering from its `apply`. The entry's text is code by default: each registrar states it in the commit that changes the marker, so a profile does not configure it. A registrar may carry the text as a factory member for evaluation, as EventHub's entry does, with the shipped text as its default. The System a branch adopted is recorded by Prompt adoption ([#2027](https://github.com/arcships/dim/issues/2027)), so a reload replays it without recomposing. The table persists nothing.

## Client-visible changes

None for app clients. Plugin authors gain one method: `compose(branch: string, base?: SystemPrompt): SystemPrompt` on `guideEntries`, declared in `npm/plugin.d.ts` beside `register`. `register` keeps its request, its reply with no value and its codes; the composed System keeps the frame, the entries and their order byte for byte; no app client reads or writes the guide. The names this contract replaces are the Rust-only readers of the composition, `compose`, `compose_system` and `changes`; their call sites are Prompt's branch part, ContextPreparation's publisher, Loop and the `dim-goal` test helper, and the supplement's migration table lists them.

## Exceptions with reasons

None.

## Supersedes

- `docs/design/plugins/README.md` and `docs/zh/design/plugins/README.md`: the Prompt row's Session column, which names the guide table, now names this contract. This PR replaces the By scope Session list's `Prompt (the entries table)` (`docs/design/plugins/README.md:56`, zh `:49`) and `Prompt（条目表）` with `PromptGuide`.

The clauses below state this owner's surface in documents this contract does not own; the owner named in each row carries the edit in that document.

| Clause | Receives |
| --- | --- |
| [Prompt](Prompt.md) the introduction before §1 and the header's scope clause, §1 Scope and non-goals, §2 The entry route, §3's lead paragraph (the composition and the frame are Prompt's own text), §3.1 The frame, §3.2 The order, §3.3 One composition, two paths, §4 The built-in entries and their writers, §5 Facts that reproduce it, §6.1 The plugin bridge, §6.2 The writers and §8 Binding appendix, and their mirror [docs/zh/design/plugins/Prompt.md](../../zh/design/plugins/Prompt.md), the same sections, §3's lead included | [PromptGuide](PromptGuide.md); Prompt adoption (scan issue [#2027](https://github.com/arcships/dim/issues/2027), batch 6, not open) turns each into a pointer paragraph and keeps the headings that have links (§2, §3.1, §3.3, §3.4, §4, §5, §6.1, §6.2, §6.3), so every existing link resolves. |
| [Prompt](Prompt.md) §3.4 Adopted when its writer loads; the zh mirror | the entry lifetime here: entries enter and leave with their writer; the adoption rules and the System move are Prompt adoption's (scan issue [#2027](https://github.com/arcships/dim/issues/2027), batch 6, not open) |
| [Prompt](Prompt.md) §6.3 ContextPreparation: the entries and the published System; the zh mirror | Prompt adoption for the adopted System (batch 6) and ContextPreparation for its publisher requirement (batch 7); both scan issue [#2027](https://github.com/arcships/dim/issues/2027), not open |
| [Prompt](Prompt.md) §7 Rule check; the zh mirror | the input-guide rows are replaced by this contract's [Rule check](#rule-check); the adoption rows are Prompt adoption's (scan issue [#2027](https://github.com/arcships/dim/issues/2027), batch 6, not open) |
| [PromptSources](PromptSources.md):5, :86, :92 and :102, [docs/zh/design/plugins/PromptSources.md](../../zh/design/plugins/PromptSources.md):5, :82, :88 and :98, and the README's PromptSources row (`docs/design/plugins/README.md:26`, zh `:25`), which assigns the state and rule entries: the frame and the route are "Prompt's", the markers Prompt writes are "explained by Prompt", and the two shared entries are PromptSources' | [PromptGuide](PromptGuide.md); PromptSources (scan issue [#2026](https://github.com/arcships/dim/issues/2026), batch 5, not open) points them here |
| [runtime-owner-operations.md](../contracts/runtime-owner-operations.md):10: `initialPromptConfiguration.get`, `promptHost.load`, `guideEntries.register`, `toolRegistry.register`, `toolSourcesView.get/observe` | PromptSources (scan issue [#2026](https://github.com/arcships/dim/issues/2026), batch 5, not open) carries its two operations, and ToolRegistry (same scan issue) carries the tool registry's and adds `guideEntries.compose` beside `guideEntries.register` in the guide cell; `observe` is crate-internal and adds no row |
| [Collaboration](Collaboration.md):22 (zh :20)'s sentence that an entry is not a factory member | Collaboration (scan issue [#2020](https://github.com/arcships/dim/issues/2020), batch 6, not open) states the qualified rule |
| [Collaboration](Collaboration.md):64 (zh :62) "the entry route is Prompt's existing contract", :65 (zh :63) "the guide entries are registrations inside Prompt's scope", :67 (zh :65) "the entries use Prompt's route", :68 (zh :66) "the guide's frame stays Prompt's", :73 (zh :71) "the registration is Prompt's language-neutral contract" and :79 (zh :77) "the entries use Prompt's `guideEntries` contract" | Collaboration (scan issue [#2020](https://github.com/arcships/dim/issues/2020), batch 6, not open) points the route at [PromptGuide](PromptGuide.md) and the frame here |
| [EventHub](EventHub.md) A.6's sentence that the entry's text is code, while the entry is a factory member, :324 (zh :319) "the guide entry uses Prompt's existing `guideEntries` contract" and A.6 (zh :439), :254 (zh :248) "Prompt's composition, order 130" and :295 (zh :290) "the guide entry it registers through Prompt's route" | EventHub (scan issue [#2023](https://github.com/arcships/dim/issues/2023), batch 6, not open) states the factory default and the shipped text, and points the entry route at [PromptGuide](PromptGuide.md) |
| [ContextCompaction](ContextCompaction.md):2083 (zh :1033) "that sentence is the frame's, owned by Prompt" | [PromptGuide](PromptGuide.md) for the frame; the document's edit is carried by SummaryStrategy (scan issue [#2025](https://github.com/arcships/dim/issues/2025), batch 2, not open) |
| [model-prompts](../model-prompts.md) §1 `:13`, §3 `:44`, §5 `:85`, §6 `:94`, §6.2 `:106-110`, §9.1 `:186-192` and §10 `:267,268,269,271,272,276,278` (zh `:266,267,268,270,271,275,277`), and the [zh mirror](../../zh/design/model-prompts.md) at the lines named: the frame, the guide mechanism and the input-guide row name Prompt; the prompt layer those lines name for the frame, the entries and the composition is this contract | [PromptGuide](PromptGuide.md) for the frame, the entries and the composition; the adoption half is Prompt adoption's (scan issue [#2027](https://github.com/arcships/dim/issues/2027), batch 6, not open); the document's edit is carried by PromptSources (scan issue [#2026](https://github.com/arcships/dim/issues/2026), batch 5, not open) |

Every other page that states a marker's entry or links `Prompt.md`'s guide sections keeps its text: each registrar that has one states its entry, and Prompt.md keeps the headings that have links.

## Verification

- A registration lives until its row unloads; an invalid marker, an empty branch id, an entry with no marker, an oversized text and a meeting duplicate are refused with their own kinds; a call outside a row answers `no_caller_row`.
- A Session row states `session` scope and a row of a branch states that branch's, as the caller's obligation; an entry lives exactly while its registering context is loaded, a mismatched branch scope stays until its row unloads, and a closing branch withdraws only the entries its rows registered.
- `compose` starts with the frame's head and ends with its tail; entries are ordered by `order` then first marker; a session entry meets every branch; two branch entries for different branches never meet; an empty branch id and one that names no branch both compose the Session's entries.
- The base is carried unchanged: a text base precedes the guide after one blank line, an empty base is the guide alone, and a parts base gains the guide as one text part with no cache control.
- `observe` starts one coalescing delivery task per subscription; it delivers the current revision as the baseline and, coalesced, later changes within the same table generation; the row's unload cancels the task and the table's shutdown aborts it rather than waiting; a composition already made does not change when an entry withdraws.
- A `register` or `observe` from a closing row, whose subtree or root closes, answers `closed` as the framework's refusal, not a Dim Session phase; a row that observes subscribes again from the next load. A composition taken after the table unloads is terminal and no caller adopts it.
- An entry becomes visible to `compose` only after its cleanup is registered, so a registration that answers `closed` never becomes visible.
- The two entries PromptGuide registers are in force for every branch, and no other part registers a competing `dim-state` or `dim-rule` entry.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part; it is not a Capability: it writes no Timeline fact and folds none. |
| §2 scopes | Session: contributors live at both scopes and the Session is their nearest common owner; an entry's scope names where it applies. |
| §3 orthogonality | No other owner's state is read; the frame, the entries and the composition are the owner's own state. |
| §4 rule 1 | No command event: a registration is an effect of its row, withdrawn at unload, and a subscription's delivery task is the table's, cancelled and awaited at unload. `closed` is the framework's refusal of that effect, so the owner reads no Session phase and needs no lifecycle dependency. |
| §4 rule 2 | No handover of an object: the reply is data and the listener is a callback. |
| §4 rule 3 | No before callback; the listener runs outside the Timeline's commit lock. |
| §4 rule 4 | Not a core mechanism: the owner is a Session part and changes no Timeline, Dispatcher or facade behavior. |
| §4 rule 5 | No batches. |
| §4 rule 6 | The composed System reaches the model through the branch's adoption and the request carrying it; this owner adds no context path. |
| §4 rule 7 | Callers use these operations; no caller reads the entry table or holds a composition object. |
| §4 rule 8 | No infrastructure change. |
| §4 rule 9 | No Timeline fact. |
| §7 values, ids and callbacks | The entry and the System are data; the listener is a callback; the branch is an id; no object crosses. |
| §7 no proxy | The table answers its own entries and composition. |
| §7.2 no owner mechanism pushed | The listener receives a table revision it may compare, and nothing else: no lock, queue, retry or change log is handed to the caller. |
| §7 one interface | One name, `guideEntries`, served by the Session dispatch, with `register` and `compose` on the public data face; `observe` is crate-internal; no service attribute and no forwarding interface. |
| §7 cross-language | `register` and `compose` take and answer data, with one code per refusal; `observe` is crate-internal because only Prompt subscribes; no method of the face is Rust-only. |
| §7 one change | The delivery replaces the Rust-only readers `compose`, `compose_system` and `changes` with `compose` and `observe`, migrates both composition callers (the adoption and the request assembly), publishes a registration only after its cleanup is registered, and updates the tests whose registrations state a scope their row does not own (`tests/integration/tests/prompt_guide.rs`, `crates/dim-agent-capabilities/src/prompt_guide.rs`'s unit tests) and the `dim-goal` helper (`crates/dim-goal/tests/support/parts.rs:1318`), in one change; the supplement's migration table lists the call sites. |
| ADR 002 D1 | Session scope. |
| ADR 002 D2 | No wiring plugin: the owner type holds the operations and the Session host dispatch serves the name. |
| ADR 002 D3 | The table is Session-scoped and its contributors are Session and branch parts, which may depend on it; nothing depends on a narrower scope. An entry follows its writer's lifetime, and the table depends on no contributor. |
| ADR 002 D4 | No setting selects a behaviour here. |
| ADR 002 D5 | The data face `register` and `compose` is language-neutral; `observe` is crate-internal; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None.

## Binding appendix

The owner type carries no service attribute: the name `guideEntries` is served by the Session host dispatch below, the form `toolRegistry` uses. `register`, `compose` and `observe` are inherent methods; `register` and `compose` are the public data face (`compose` is also the Rust binding the flow tests read), and `observe` is `pub(crate)`. The part provides the owner for in-process callers and this dispatch for the bridge, one provider per key. `caller: &Ctx` is the caller parameter for `register` and `observe`; the listener is a direct parameter, not a request field.

```rust
use dim_agent_runtime::names::name_of;
use dim_agent_types::SystemPrompt;
use rutis::host::{CtxHost, InstanceId, Key};
use rutis::Ctx;
use rutis_bridge::exports::failure;
use rutis_bridge::session::{arg, caller_row, host_key_in, Error as BridgeError, HostDispatch, Reply, Value};
use serde_json::{json, Value as Json};
use std::sync::Arc;

/// The Session's guide entries, the frame and the composition over them.
/// Provided in the Session by the Prompt plugin's `PromptGuide` part.
pub struct GuideEntries { /* registrations by (scope, first marker) */ }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuideScope {
    Session,
    Branch(String),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuideEntry {
    pub markers: Vec<String>,
    pub order: i32,
    pub text: String,
}

#[derive(Debug, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum GuideError {
    #[error("{message}")]
    InvalidEntry { message: String },
    #[error("the registration has no caller row")]
    NoCallerRow,
    #[error("{message}")]
    DuplicateMarker { message: String },
    #[error("{message}")]
    TooLarge { message: String },
    #[error("the calling row's subtree or root is closing")]
    Closed,
}

impl GuideEntries {
    /// Publishes `entry` for `scope` for the calling row's lifetime; a framework refusal because
    /// the calling row's subtree or root closes answers `Closed`.
    pub fn register(&self, caller: &Ctx, scope: GuideScope, entry: GuideEntry) -> Result<(), GuideError> { /* ... */ }

    /// The System for `branch` over `base`.
    pub fn compose(&self, branch: String, base: Option<SystemPrompt>) -> SystemPrompt { /* ... */ }

    /// Delivers the table revision now and on each change until the calling row unloads.
    pub(crate) async fn observe(
        &self,
        caller: &Ctx,
        listener: impl Fn(u64) + Send + Sync + 'static,
    ) -> Result<(), GuideError> { /* ... */ }
}

/// `register` and `compose` for other languages: provided at `host_key_in(&name_of::<GuideEntries>(), session)`
/// in the Session's subtree, the form `toolRegistry` uses.
struct ForeignGuideEntries(Arc<GuideEntries>);

impl HostDispatch for ForeignGuideEntries {
    fn methods(&self) -> Option<Json> {
        Some(json!({ "register": "async", "compose": "sync" }))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        match method {
            "compose" => {
                let (branch, base) = compose_request(args)?;
                arg(&self.0.compose(branch, base))
            }
            "register" => {
                // Read the caller inside `invoke`: a future returned here no longer sees it.
                let caller = caller_row()
                    .map(|row| row.ctx().clone())
                    .ok_or(GuideError::NoCallerRow)?;
                let (scope, entry) = request(args)?;
                let owner = self.0.clone();
                Ok(Value::future(async move {
                    owner.register(&caller, scope, entry).map(|()| Value::Undefined).map_err(BridgeError::from)
                }))
            }
            other => Err(BridgeError::value(format!("guideEntries has no method {other}"))),
        }
    }
}

// The Session part's `apply` provides: the owner at its key for in-process rows, and the dispatch at
// its host key for the bridge, one provider per key. The two built-in registrations and the
// delivery-task effects are omitted here.
async fn apply(
    ctx: &Ctx,
    session: InstanceId,
    table_key: Key<GuideEntries>,
    table: Arc<GuideEntries>,
) -> rutis::Result<()> {
    ctx.provide_at::<dyn HostDispatch>(
        host_key_in(&name_of::<GuideEntries>(), session),
        Arc::new(ForeignGuideEntries(table.clone())),
    )?;
    ctx.provide_at(table_key, table)?;
    Ok(())
}

/// One wire code per `GuideError` variant.
impl From<GuideError> for BridgeError {
    fn from(error: GuideError) -> Self {
        let code = match &error {
            GuideError::InvalidEntry { .. } => "invalid_entry",
            GuideError::NoCallerRow => "no_caller_row",
            GuideError::DuplicateMarker { .. } => "duplicate_marker",
            GuideError::TooLarge { .. } => "too_large",
            GuideError::Closed => "closed",
        };
        failure("GuideEntryError", code, error.to_string(), None)
    }
}
```

`request` decodes the bridge's `{scope, entry}` arguments and `compose_request` its `{branch, base?}` into the data types above; `arg` is the bridge's serde reply.

The `GuideError` variants serialize to the codes in the error table.
