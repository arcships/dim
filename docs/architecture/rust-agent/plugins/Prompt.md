# Prompt

Owner: [prompt.rs](../../../../crates/dim-agent-capabilities/src/prompt.rs) and
[prompt_state.rs](../../../../crates/dim-agent-capabilities/src/prompt_state.rs).
Loads and adopts prompt/model/tool configuration in timeline order, exposes
ConfigureControl and projects adopted prompt and configuration state.
`Branch::get<PromptSnapshot>()` reads the last adopted `context/configured` without
replaying history; rebuilding restores it from checkpoint configuration and facts.

## Dependencies

| Service | Purpose |
| --- | --- |
| `B:ConfigurationPublisher` | Atomically publish configuration and final input content. |
| `S:PromptHost` | Load named prompt sources. |
| `S:InitialPromptConfiguration` | Initial source/model selection. |
| `B:BranchLifecycle` | Coordinate opening and closing. |
| `B:TimelineIdentity` | Session and branch ids of the `prompt.load` span. |
| `B:Dispatcher` | Order configuration adoption and state projection. |
| `B:Timeline` | Write controls and rebuild adopted context. |
| `S:FaultReporter` | Report owned work failures. |
| `B:BranchOpening` | Register initial prompt completion receipt. |
| `S:GuideEntries` | The entries whose load or unload moves this branch's composition. |

## Events and lifetime

Consumes prompt configure-request/result/changed, `context/configured`, turn
boundaries, branch start/resume, `context/scope`, `stop/interrupt`. Produces
configure controls and, through ConfigurationPublisher, prompt changes,
context configuration and final adopted `input/message` content. Scope/turn
changes can produce retirement inputs. A tracked adoption worker serializes
work; opening handlers register receipts and return. Cleanup cancels pending
loads, joins tasks and settles waiters. Constructed host-load futures are joined
even after cancellation. Reload projects committed context; runtime waiter IDs
do not resume host operations. Source loading alone is not adoption.

`Prompt::configure_loaded` accepts frozen source values and an optional model on
the existing branch service. It derives targets, registers values in the existing
pending operation and writes the same configure-request/result commands; it uses
the same supersession, serial adoption and atomic publication. A source entry has
its target's identity, guaranteed by the producer. A missing entry revokes that
target. Dropping the reply waiter does not revoke a committed command. The
[model configuration contract](../../../design/model-configuration.md) records the
owners, rule check and complete-candidate concurrency limit.

## Tests

[prompt_lifecycle.rs](../../../../tests/integration/tests/prompt_lifecycle.rs):
`configuration_is_atomic_failure_safe_and_inherited`,
`overlapping_load_is_superseded_and_close_joins_cancellation`,
`startup_opening_dispatch_is_free_and_close_or_deadline_joins_the_host`.

<!-- generated:plugin Prompt — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:BranchLifecycle`、`B:BranchOpening`、`B:ConfigurationPublisher`、`B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`S:FaultReporter`、`S:GuideEntries`、`S:InitialPromptConfiguration`、`S:PromptHost` |
| 提供 | `B:Prompt`、`B:StateCell<PromptSnapshot>`、`B:dyn ConfigureControl` |
| 被注入于 | `PromptSourcesBranch`、`SkillsBranch`、`ToolBinding` |
<!-- /generated -->

## PromptGuide

Owner: [prompt_guide.rs](../../../../crates/dim-agent-capabilities/src/prompt_guide.rs).
Holds the Session's input-guide entries, the frame and the composition over
them, and registers the two entries of the markers Prompt itself writes,
`dim-state` and `dim-rule`, from its own `apply`
([Prompt design §2 and §3](../../../design/plugins/Prompt.md#2-the-entry-route)).
Each other writer registers its own entries; the composition orders what is in
force for a branch by `order` and first marker, so neither load order nor
registration order enters.

### Dependencies

None: it provides the table and owns its two entries.

### Events and lifetime

A registration is an effect of the part that made it: it is withdrawn when that
part unloads, and a reloaded part replaces its entry instead of doubling it. A
reload that changes an entry's text withdraws the old entry and registers the
new one in that order, so a branch whose adoption runs between the two publishes
the composition twice — without the entry, then with the new text — and runs
with the intermediate System for a step; the design accepts that. A
composition already made is unaffected by a later registration or withdrawal on
its own: the adopted System moves only at an adoption that publishes a
configuration. Prompt's branch part makes the load or unload one of them:
before the branch's own open adoption composes it takes the entries'
`changes()` watch and the composition it starts from, and starts listening
before that adoption runs, so a change during the adoption is queued behind
the open, whose own reconciliation still comes first; on every change it
composes the branch's guide again and adopts it with the settings unchanged
on every open branch whose composition moved, a registration made while
the open runs included. A branch whose composition did not move reads
nothing, because the composition reads the in-memory entries only. One more
adoption runs inside every open, over the configuration the branch carries
and before it reports itself open, so a fork and a restore converge on the
entries in force at load and their first request carries them; it costs one
context read per open and publishes nothing when the adopted System already
equals the composition. The listener ends when the branch starts closing,
and an adoption that already started reading refuses its append then, so
entries that withdraw only because the branch shuts down are never recorded.
A writer's tools can reach a request one step before its entry: the tools bind through the tool adoption as soon as their source loads,
while the entry arrives through the guide adoption and its queue, so a step
between the two carries the new tools with the previous System. A tools-only or
model-only adoption re-renders the guide over the source text the configuration
records, and one written before the source was recorded keeps its adopted
System. Entries reach the model only through the System a branch adopted or
through the request Loop assembles, never through a request of their own.

### Tests

[prompt_guide.rs](../../../../tests/integration/tests/prompt_guide.rs):
`both_paths_carry_one_composition`,
`an_adopted_request_and_the_summarizers_request_carry_one_system`,
`a_tools_only_adoption_carries_the_entry_its_writer_registers`,
`an_entry_takes_effect_when_its_writer_loads_and_ends_when_it_unloads`,
`a_branch_scoped_entry_publishes_only_on_its_branch`,
`an_entry_registered_while_its_branch_opens_is_adopted`,
`a_fork_composes_its_own_entries_at_its_open`,
`a_fork_carries_its_own_entry_in_its_first_request`,
`a_restored_branch_converges_on_the_entries_its_writers_register`,
`a_restored_branch_carries_its_entries_in_its_first_request`,
`a_guide_adoption_racing_its_branch_close_records_nothing`,
`an_entry_withdrawing_as_the_session_unloads_is_not_recorded`,
`a_reloaded_writer_that_keeps_its_entry_records_nothing`,
`a_composition_that_moved_and_returned_while_its_branch_opens_is_adopted`,
`an_entry_that_returns_during_the_open_adoption_is_adopted`,
`each_branch_keeps_its_own_entries`, `the_frame_states_the_compaction_handle`;
[reload.rs](../../../../crates/dim-app-server/tests/transport_flow/reload.rs):
`an_entry_leaves_with_its_contributor`.

<!-- generated:plugin PromptGuide — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | — |
| 提供 | `S:GuideEntries`、`S:dyn HostDispatch#guideEntries` |
| 被注入于 | `BackgroundExec`、`BranchLine`、`ContextPreparation`、`Loop`、`Prompt`、`TimelineTree` |
<!-- /generated -->
