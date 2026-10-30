# Prompt

Status: Draft ([#548](https://github.com/arcships/dim/issues/548)). Requirements: [model-facing prompts §6 and §9.1](../model-prompts.md#6-the-input-guide). Rules: [design rules](../../architecture/rust-agent/principles.md) §1, §4 and §5, [ADR 002](../../adr/002-plugin-scope.md). Scope: branch, plus the entries table its Session holds. Language: Rust. The current capability is on [Prompt](../../architecture/rust-agent/plugins/Prompt.md)'s capability page.

Prompt owns the input guide: the text every request carries about the model's own input. It owns the guide's frame, composes the frame and the entries in force into one text, and carries the entries each capability contributes. It owns none of the entries' words: the capability that writes a marker contributes the entry that explains it. The frame states the channels and their trust classes, the channel rule, escaping and the restatement after a summary, so the model can tell the operator's words from the runtime's, and both from material.

## 1. Scope and non-goals

Prompt owns:

- **the frame** — the channels, the trust classes, the channel rule and the restatement after a summary ([model-facing prompts §6.2](../model-prompts.md#62-who-owns-which-part)); no scope removes or replaces it;
- **the entries table**, in the Session, and the route by which a loaded capability registers an entry (§2);
- **the composition** — the frame and the entries in force in one deterministic order (§3);
- **the channel rule's `[input <id>]` exception**, worded as a runtime guarantee and therefore protocol (§3.1).

Not Prompt's:

- an entry's words belong to the capability that writes the marker (§6);
- the request shape belongs to Loop, the adopted configuration to ContextPreparation, the summary path to ContextPolicy;
- the state and rule sources and the words the model reads in them belong to PromptSources; the two markers are Prompt's, which writes them (§4).

Not this design: each capability's list of model-facing texts with its protocol and policy marks, and the flow assertion in that writer's own flow ([#551](../model-prompts.md#92-every-capability-that-writes-model-facing-text), [#552](../model-prompts.md#94-timelinetree), [#553](../model-prompts.md#95-goal), [#554](../model-prompts.md#96-contextcompaction)); removing Prompt's two call sites ([#555](../model-prompts.md#97-loop-and-contextpreparation)).

## 2. The entry route

The route is a contract in the form [ADR 002 D5](../../adr/002-plugin-scope.md#d5-contracts-are-language-neutral) gives: identity, request and reply as data, errors, cancellation and unload. Rust is one binding of it ([§8](#8-binding-appendix)); a plugin contributed through the plugin bridge registers through the same contract (§6.1).

**Identity.** A registration belongs to two things: the loaded part that makes it, and the markers its entry explains. The contributor is taken from the call — its `ctx` in Rust, the calling row through the bridge — and never from the payload, so no caller can register in another's name. The entry is in force exactly while that part is loaded ([rutis usage R6](../rutis-usage.md#r6-contributions-register-with-their-owner)).

**Scope.** The table is the Session's, because contributors live at both Session and branch scope and the Session is their nearest common owner, the same reason its prompt sources are the Session's. Each registration states where its entry applies: `session` for every branch of the Session, or one branch id for that branch alone. A branch-scoped entry is in force exactly while that branch's part is loaded, so closing one line withdraws its entries and leaves the other lines' composition untouched.

**Uniqueness is per composition.** For any one branch, a marker has at most one entry in force. A session-scoped entry therefore collides with every entry for the same marker; two branch-scoped entries for one marker collide only when they name the same branch. A registration is keyed by its scope and its first marker, so two branches each register their own copy of a branch-scoped entry without meeting.

**Request.** One registration: the scope and one entry.

| Field | Type | Meaning |
| --- | --- | --- |
| `scope` | `"session"` or `{ "branch": <branch id> }` | Where the entry applies |
| `entry.markers` | list of strings, at least one | The marker element names this entry explains, e.g. `["dim-branch-line", "dim-branch-lines"]` |
| `entry.order` | integer | The entry's position among the entries ([§3.2](#32-the-order)) |
| `entry.text` | string | The entry, verbatim, as the model reads it |

**Reply.** `null` when the entry is registered. The reply carries no identity of its own: the caller already knows what it registered, and the registry keeps no second key for it.

**An entry from a writer that is not loaded cannot be made.** The registration is made by the loaded part itself, tied to its own generation, so there is no path by which a registration outlives its writer or arrives from one that never loaded. The one case in which the writer's identity is not available is a call from another language whose calling row cannot be resolved; that call fails with `no_caller_row` rather than registering in no one's name.

**Errors** are data, `{code, message}`:

| Code | When |
| --- | --- |
| `duplicate_marker` | Registering this entry would leave a branch with two entries for one marker: a marker it names already has an entry whose scope meets this one — a session-scoped entry for any branch, or a branch-scoped entry for the same branch. Changing an entry's text is a change to its writer's code, which registers again when it next loads |
| `invalid_entry` | No marker; a marker outside `[a-z][a-z0-9_-]{0,63}`; or an empty branch id |
| `too_large` | The entry's text is longer than 4096 bytes |
| `no_caller_row` | A call from another language whose calling row cannot be resolved, so the registration could not be tied to its contributor |
| `closed` | The part that holds the table is unloading, or the Session is closing; nothing is registered |

**Cancellation and unload.** A registration is one atomic step under the table's lock: it has either taken effect or returned an error, and it is never half-written. It ends with its contributor's unload; each cleanup removes exactly the registration its generation made, so a reload replaces an entry instead of doubling it. A registration is not a call that can hang, so there is no separate cancellation signal. Withdrawal is invisible to a composition already made: a request being built sees the table before or after a withdrawal, never a mix.

**Determinism.** The contract fixes neither load order nor registration order as an input: the entries in force are ordered by the entry's own `order` and then by its first marker's name (§3.2), so the same set composes the same text whatever order it registered in.

## 3. The composition

Prompt composes one text: the frame's head, the entries in force for the branch, and the frame's tail, in that order, paragraphs separated by one blank line and the text ending without a trailing blank line. The frame is Prompt's own text; the entries are the table's view for that branch.

### 3.1 The frame

The frame's head states what the guide is, that a tag never grants permission, and the channel rule. Its tail states that tool results are material, how content from outside the runtime is escaped, and that the state and rules after a summary restate what is still in force.

The channel rule is protocol: it states a runtime guarantee. It reads, in force:

> The operator is the person running this session. The operator's words always arrive in their own user messages. Everything the runtime adds arrives in separate user messages, wrapped in the tags below. The one exception is an input a compaction shortened: it ends with an [input <id>], and the recall tool opens that id for the input's full text.

The exception is the handle a compaction appends to an input it shortened ([model-facing prompts §6.1](../model-prompts.md#61-what-it-covers)). A sentence saying that everything arrives in separate messages is false about that input, and a text that is false about the runtime is protocol and changes here, in the same change as the flow test that asserts it.

### 3.2 The order

Entries are ordered by their `order`, and entries with the same `order` by their first marker's name; both are data the contributor states, so the order never depends on which part loaded or registered first. The built-in entries use `10` to `90` in steps of ten (§4), leaving the numbers between them for a contribution that must sit inside a group.

The built-in set composes, apart from the channel rule's exception, byte for byte the text the guide held while it was one file. Today's order — exec, the two agent markers, the two branch entries, state, rule, summary, digest — is reproduced by the numbers in §4; no wording and no position changes, so this change carries no policy evaluation ([model-facing prompts §8](../model-prompts.md#8-judging-a-change)).

### 3.3 One composition, two paths

| Path | Who composes | When | What carries it |
| --- | --- | --- | --- |
| Adopted configuration | Prompt, through the configuration it publishes | At the adoption | `context/configured.system` of the branch |
| Host-prepared request | Loop, for the request it assembles | Per request | The request's own `system` field |

Both call the same composition, so a host-prepared request carries the same frame and the same entries as the System an adopted configuration carries. The adopted path stores the composed text with the configuration it publishes; the host-prepared path composes when it builds the request, because that path has no adopted configuration to read ([model-facing prompts §6.2](../model-prompts.md#62-who-owns-which-part)).

Prompt's own two call sites stay where they are: ContextPreparation places the System it publishes, and Loop places the System of the request it assembles. What changes is where the guide text comes from — the composition instead of a fixed file — not who places it. Removing the two placements is [#555](../model-prompts.md#97-loop-and-contextpreparation)'s.

### 3.4 Adopted when its writer loads

A guide entry takes effect when its writer loads and ends when its writer unloads. Prompt adopts the guide there: every open branch whose composition moved publishes one `context/configured` with its settings — the model, the tools and the source text — unchanged, and with the System composed over the entries in force; a branch whose composition did not move neither publishes nor reads its context for that change. Between those points the guide does not change, because an entry reaches the table only from its writer's `apply` and leaves with that writer's generation.

A branch also adopts the guide once at its open: a fork inherits its parent's configuration and a restored branch carries the System it persisted, while the entries in force are the ones its writers registered at load. That adoption is part of the open: the branch reports itself open only after the adoption published, so the first request after an open carries the reconciled entries and none assembles from the inherited or the persisted System. It costs one context read per open and publishes nothing when the composition already equals the adopted System.

A closing branch adopts nothing: its writers withdraw because it is shutting down, and a configuration recorded then would carry a System the branch never ran with.

The host-prepared path changes the guide at the same points without publishing anything, because it composes the guide into the request it assembles. A branch started after a load composes the entries in force at its open, and a request built from an adopted configuration carries the System that branch adopted.

No request assembles a guide of its own, and no contributor appends text to a request. That is what keeps the guide from forking a cached prefix in the middle of a Session: the guide is part of the prefix, and it moves only when the prefix legitimately moves.

## 4. The built-in entries and their writers

Each built-in entry moves verbatim to the capability that writes its markers, and is registered from that part's `apply`. The registrar is the part whose lifetime the marker's presence follows.

| Order | Markers | Registrar | Scope | Entry |
| --- | --- | --- | --- | --- |
| 10 | `dim-notification` | BackgroundExec | branch | The exec notification |
| 20 | `dim-agent-report` | Collaboration's Session part | session | The report of delegated work |
| 30 | `dim-agent-message` | Collaboration's Session part | session | A message from another agent |
| 40 | `dim-branch-line` | TimelineTree | session | Which line of the Session this is |
| 40 | `dim-branch-lines` | BranchLine | branch | The roster on `main`; the tie on `order` leaves the identity's entry first |
| 50 | `dim-branch-message`, `dim-branch-idle`, `dim-branch-ended` | BranchLine | branch | A message from another line, and the runtime's reports about a line |
| 60 | `dim-state` | PromptGuide | session | What a state block is |
| 70 | `dim-rule` | PromptGuide | session | What a rule block is, and how it ends |
| 80 | `dim-summary` | ContextCompaction's summary strategy part | session | What a summary is |
| 90 | `dim-digest` | ContextCompaction's context-preparation part | branch | What a digest is |

The registrar is the part that writes the marker, not the part that authors the source's words. Prompt turns any `Context` or `State` source into a `<dim-rule>` or `<dim-state>` input, so those two markers are Prompt's even when the source's words are PromptSources'. A profile that installs Prompt without PromptSources can still be handed a context source, and the markers it writes need their entries.

A compaction's replacement belongs to the policy every strategy is called through, not to a strategy: the policy composes the digest after any strategy returns ([ContextCompaction §5.10](ContextCompaction.md#510-class-3a-the-digest)). The entry therefore registers with the part that binds the policy, and only while a strategy is bound — a Session with no strategy compacts nothing and writes no digest. A Session with a strategy other than the product one still sends `<dim-digest>`, and its guide explains it.

A capability that writes a marker and registers no entry is a capability the guide does not explain; a capability that registers an entry for a marker nothing writes leaves a promise the runtime does not keep. Both are the writer's to fix, and [#551](../model-prompts.md#92-every-capability-that-writes-model-facing-text) makes each writer assert the pair in its own flow.

## 5. Facts that reproduce it

The entries in force are reproducible after a restart or a reload, as the other prompt sources are.

| Fact | Where | What it reproduces |
| --- | --- | --- |
| The assembly | The loaded parts of the Session and its branches | Which capabilities are loaded, and so which entries each `apply` registers again |
| The adopted System | `context/configured.system` on the branch | The guide exactly as the branch adopted it, replayable without recomposing |

The entries' text is code: each registrar states it in the commit that changes the marker. A restart reloads the same assembly, so the same entries register, and the branch's adopted System replays from the Timeline; a later adoption composes from the same registrations and produces the same text, because the order is the entries' own data. What a reload changes is a writer's availability, not the recorded text of an adoption already made.

The state and rules a branch adopted are not facts about the guide: they are prompt entries, recorded by `prompt/changed` like any other source. The guide names them; it does not carry their content.

## 6. Cross-module requirements

### 6.1 The plugin bridge

A plugin contributed through the plugin bridge registers an entry the same way a Rust part does: from its `apply`, for as long as its row is loaded, through a call whose arguments and reply are serde data. The route reaches other languages in three parts:

1. **The name, shared in the Session group.** A Rust registry of this shape reaches other languages only when its name is shared where it is provided; the host that runs foreign parts lists those names (`share_names` in the App Server), and `guideEntries` is shared there beside `toolRegistry`.
2. **A dispatch that ties the registration to the caller.** The Rust `register` takes its `ctx` from the caller; a foreign caller has no `ctx`, so the name is served through a `rutis_bridge` `HostDispatch` that reads `rutis_bridge::session::caller_row()` and registers under that row — the form `toolRegistry` uses. Without a caller row the call fails with `no_caller_row` rather than registering in no one's name.
3. **The TypeScript declaration.** The interface joins `npm/plugin.d.ts` beside `ToolRegistry`'s, naming the request, the reply and the error codes above: `register(scope, entry)` where `scope` is `"session"` or `{"branch": "<branch id>"}` and `entry` is `{markers, order, text}`; the call resolves with no value; a refusal throws a `GuideEntryError` whose `code` is one of §2's codes, `closed` included, and `no_caller_row` for a call made outside a row.

The capability itself — the table, the composition and the two placements — works in Rust alone, so the entries' route is complete for every built-in writer without the bridge, and the bridge makes it complete for the writers that live in another language.

### 6.2 The writers

Each capability in §4 adds one dependency, its Session's `guideEntries`, and registers its entries from `apply` — Prompt's own `PromptGuide` part included. None of them needs a new service, event or configuration: the registration is a call to the owner, the way a tool registers with `toolRegistry` and a prompt source with `promptRegistry`.

### 6.3 ContextPreparation: the entries and the published System

The target in §3.4 needs one change in [ContextPreparation](../../architecture/rust-agent/plugins/ContextPreparation.md)'s publisher ([#555](../model-prompts.md#97-loop-and-contextpreparation)) and one in Prompt. The publisher composes the guide at every adoption that publishes a System: a prompt change that renders the System composes the new source text with the entries in force, and a tools-only, model-only or guide adoption re-renders the guide over the source text the branch adopted.

- The published configuration records that source text apart from the guide it carried (`context/configured.system_source`), so the source stays what the branch adopted even as the guide is composed again.
- A tools-only or model-only adoption, and a guide adoption, compose the guide as it reads now over the recorded source text: a writer that loads has its entry in force at the load itself, whether or not that load also adopts its tools, and one whose composition already reads so publishes nothing.
- A configuration written before the source was recorded has none to re-render from: it keeps its adopted System, guide included, and only a later adoption that renders the System replaces it. `restoring_a_session_keeps_its_adopted_system_and_model` (`tests/integration/tests/prompt_lifecycle.rs`) restores such a System from `context/configured`; recomposing it from the snapshot's prompt sources alone would put the guide where the adopted System stands.
- The request reads the adopted System as it is: Loop carries `context/configured.system` into a request built from it, and a host-prepared request carries the composition Prompt composes. Neither appends text of its own.

The trigger is Prompt's. Its branch part declares a read of the table and subscribes to a `changes()` the table forwards from its registrations, so the watch stays with the registry the entries live in rather than crossing into ContextPreparation. The listener takes its version and the composition it starts from before the branch's own open adoption composes, and starts before that adoption runs: a registration made while the open runs therefore moves the composition the listener compares against and is adopted, instead of entering a baseline taken afterwards and marked as seen, and a change made while that adoption runs is adopted by an adoption queued behind the open's arm. Each change after it composes the branch's guide again in memory, which reads neither the Timeline nor the store, and enqueues one guide adoption on the same serial queue a model change and a tool adoption use only when that text moved. The open runs one more adoption itself, before it reports itself open, because the baseline is a composition and not the System the branch carries: it costs one context read per open and publishes nothing when the two already agree. Running it on the open's path rather than as a later queue item is what makes a fork's and a restore's first request carry the entries in force. Nothing is recorded on a closing branch: the listener ends when the branch starts closing, and an adoption that already started reading refuses its append under the commit lock, so the item is dropped with the others rather than persisted as a System the branch never ran with.

A writer's tools can reach a request one step before its guide. The tools arrive through the tool adoption as soon as their source loads, while the entry arrives through the listener and this queue, so a step in that window carries the new tools with the previous System and the next step replaces it: the prefix moves twice where §6.2 describes one move. The design accepts that step. Making Loop wait for a pending guide adoption would couple it to Prompt's queue for one step's prefix, and the guide still changes only at an adoption, never inside the request.

## 7. Rule check

| Rule | How this design complies |
| --- | --- |
| §1 Plugins and capabilities bounded | The frame is Prompt's; each entry is its writer's. The table is a carrier: Prompt holds the entries and owns none of their words |
| §3 Orthogonality | The route is one more contribution owner, of the shape already used by `toolRegistry`, `promptRegistry` and `sessionMethods`. It carries no marker's business meaning, and any capability uses it from Rust, and later from the bridge |
| §4 rule 1 Apply registers, cleanup releases | A contributor registers from `apply`; the registration ends with that part's generation |
| §4 rule 2 Facts and services | No new event and no new fact: the composition is carried by the adopted configuration and by the request, both existing |
| §4 rule 4 No business meaning in core | Timeline and Dispatcher gain nothing; the table is a capability's own state |
| §4 rule 6 Context from consumed events | The guide still reaches the model through the adopted configuration or the host-prepared request; this design adds no context path |
| §4 rule 7 Public events and services | `guideEntries` is a contribution owner in the Session, like `promptRegistry`; it is not a port, a checkpoint or an adapter |
| §4 rule 8 Infrastructure unchanged | None is changed. §6.1 raises what the bridge needs; it is not built here |
| §5 Ownership | Each text stays with its author, and each requirement sits inside one owner |
| ADR 002 D1 Scope | The frame is Prompt's; entries live where their writers live, and the table sits at the Session, the contributors' nearest common owner |
| D2 No wiring modules | None is added; the composition is Prompt's own code |
| D3 Scope dependencies | The table is Session-scoped and its contributors are Session and branch parts, which may depend on it; nothing depends on a narrower scope |
| D4 Configuration | None is added |
| D5 Language-neutral contracts | The route is stated as identity, data, errors, cancellation and unload, with Rust as one binding; §6.1 names what the bridge needs to be the second |
| ADR 008 D2 No delivery status | The document states the design only |
| ADR 008 D3 Own scope only | Each writer's own texts and their classes stay on that writer's page |

## 8. Binding appendix

Rust is one binding of §2 and §3. Names and semantics are above; this section is the spelling.

```rust
/// One entry of the input guide: the markers it explains and its text.
pub struct GuideEntry {
    /// The marker element names this entry explains, e.g. `dim-branch-line`.
    pub markers: Vec<String>,
    /// Position among the entries; ties break by the first marker's name.
    pub order: i32,
    /// The entry, verbatim, as the model reads it.
    pub text: String,
}

/// Where a registration applies.
pub enum GuideScope {
    /// Every branch of the Session.
    Session,
    /// One branch, by its timeline id.
    Branch(String),
}

/// The Session's guide entries, and the composition over them. Provided in
/// the Session by the Prompt plugin's `PromptGuide` part.
pub struct GuideEntries { /* registrations, keyed by (scope, first marker) */ }

impl GuideEntries {
    /// Registers `entry` for `scope` on behalf of the part that owns `ctx`,
    /// until that part unloads. Fails when the registration would leave a
    /// branch with two entries for one marker.
    pub fn register(&self, ctx: &Ctx, scope: GuideScope, entry: GuideEntry) -> Result<(), Error>;

    /// The guide for `branch`: the frame, the entries in force, and the
    /// frame's tail, paragraphs joined by one blank line.
    pub fn compose(&self, branch: &str) -> String;

    /// The System for `branch`: `base` text followed by the guide. A missing
    /// or empty base is the guide alone, and a `Parts` base gains the guide
    /// as one more text part.
    pub fn compose_system(&self, branch: &str, base: Option<SystemPrompt>) -> SystemPrompt;
}
```

A refusal is a small error type, the source of the `PluginFailed` that `rutis::Error` carries, so a bridge reads its code from the downcast and never from the message:

```rust
/// The codes the table decides (§2).
pub enum GuideEntryErrorCode {
    /// `invalid_entry`
    InvalidEntry,
    /// `duplicate_marker`
    DuplicateMarker,
    /// `too_large`
    TooLarge,
}

impl GuideEntryErrorCode {
    /// The code as §2 spells it.
    pub fn as_str(self) -> &'static str;
}

/// A refused registration: its §2 code and the message for the log.
pub struct GuideEntryError {
    pub code: GuideEntryErrorCode,
    pub message: String,
}
```

`invalid_entry`, `duplicate_marker` and `too_large` are the codes this binding produces. `no_caller_row` is produced only by the bridge: a Rust caller always has its `ctx`, and the registration is that caller's own effect. `closed` comes from the framework: when the caller's context is closing or unloading, the effect cannot be registered and the framework returns its own closed error, which a bridge maps to `closed`; the Rust binding returns it unchanged rather than dressing it as a `GuideEntryError`.

Prompt composes through `GuideEntries::compose_system`, at the adoption it publishes and, for a host-prepared request, where Loop assembles its System: the composition is the `SystemPrompt` the request carries, placed over the source text the branch adopted. No caller appends text of its own.

The foreign form of `register` is §2's data: the scope, the entry, and the error codes; `compose` is Rust-only, since no other language reads a composition — every language reaches the model through the same request.
