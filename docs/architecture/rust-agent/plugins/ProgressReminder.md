# ProgressReminder

Owner: [progress_reminder.rs](../../../../crates/dim-agent-capabilities/src/progress_reminder.rs).
Asks the model for reply text after a long silent stretch of tool calls. The
reminder is a branch-scoped, unkeyed Append input: it rides with the line's next
model request and never stays in effect.

## Dependencies

| Service | Purpose |
| --- | --- |
| `B:BranchLifecycle` | Start the writer only after the line is published. |
| `B:Dispatcher` | Consume the line's turn, text and tool facts in order. |
| `B:Timeline` | Read the open turn and its latest text when the part rebuilds. |
| `B:TimelineIdentity` | The line the reminder is written on and the scope of its guide entry. |
| `S:SessionConfig` | `interactive`: a non-interactive Session gets no reminder. |
| `S:SessionLifecycle` | Start the writer only after the Session is published. |
| `S:TreeCommands` | Write the reminder through the line's validating input. |
| `S:GuideEntries` | Register the `<dim-progress-reminder>` entry for the line, until this part unloads. |

<!-- generated:plugin ProgressReminder — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | 分支 |
| 注入 | `B:BranchLifecycle`、`B:Dispatcher`、`B:Timeline`、`B:TimelineIdentity`、`S:GuideEntries`、`S:SessionConfig`、`S:SessionLifecycle`、`S:TreeCommands` |
| 提供 | — |
| 被注入于 | — |
<!-- /generated -->

## Events and lifetime

Consumes `turn/start`, `turn/end`, `step/text` and `step/tool-use`; produces
one `input/message`. A child event is not the line's own reply and is ignored. A
stretch starts at the turn's start, the latest text or the latest reminder,
whichever is later, and the model must have made a tool call since then. When
the stretch reaches `ProgressReminderConfig::after` (60 seconds by default), an
owned task rechecks the stretch and writes
`<dim-progress-reminder seconds="N">` as an Append. Writing text resets the
stretch; a reminder starts a new one, so the next needs a new tool call from the
model and another full threshold. The task waits on the runtime clock through
`tokio::time::sleep_until`, so a test drives it with the paused clock. No
reminder is written at a turn's start, between turns, or in a Session whose
configuration has `interactive = false`.

The rebuild reads the committed facts before live delivery: the latest turn
boundary, the latest text and the latest reminder of an open turn, the latest of
them by position as the anchor, and whether the model made a tool call after
that anchor. So a reload continues the same stretch and writes nothing twice.

## Model-facing texts

The marker and its `seconds` attribute are protocol: the guide entry names the
element and what its number means. The block's sentence and the entry's
instruction to tell the user what it is doing are policy
([model-facing prompts §4](../../../design/model-prompts.md#4-protocol-and-policy)).
The entry registers at Branch scope; every line loads this part, so every line's
guide carries the same text ([Prompt §4](../../../design/plugins/Prompt.md#4-the-built-in-entries-and-their-writers)).
A Session with `interactive = false` registers neither the entry nor the writer,
because its blocks never appear.

## Tests

[progress_reminder_flow.rs](../../../../crates/dim-agent-capabilities/tests/progress_reminder_flow.rs):
`one_reminder_after_the_threshold_arrives_as_an_append`,
`text_within_the_window_earns_no_reminder`,
`a_later_text_resets_the_stretch`,
`a_long_tool_call_earns_one_reminder`,
`a_new_tool_call_earns_the_next_reminder`,
`a_non_interactive_session_gets_none`,
`a_reload_writes_no_second_reminder`.

[progress_reminder.rs](../../../../crates/dim-agent-capabilities/src/progress_reminder.rs):
`a_rebuilt_stretch_needs_a_tool_call_after_the_reminder`.
