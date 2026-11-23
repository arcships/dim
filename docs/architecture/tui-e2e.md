# Terminal end-to-end suite

The native terminal UI's end-to-end scenarios run through tmux on a developer's
machine. They drive the production CLI and its App Server with real terminal
input, resizes and captured output, against a deterministic model fixture. CI
does not run a migrated flow: that case is `#[ignore]`d here and runs only in
this suite. A TUI case with no tmux scenario yet stays in CI until one, or a
component or protocol test, proves the same behavior; the remainder is tracked
in #1810.

## Command

```sh
cargo build --locked -p dimcode -p dim-app-server
node scripts/tui-tmux/run.mjs
```

Options and useful forms:

```sh
node scripts/tui-tmux/run.mjs --list                       # areas and scenario names
node scripts/tui-tmux/run.mjs --area approvals             # one area
node scripts/tui-tmux/run.mjs queue_holds_steers_and_deletes
node scripts/tui-tmux/run.mjs --keep --binary target/debug/dimcode
node scripts/tui-tmux/run.mjs --artifacts /tmp/tui-failure # failed cases go here
```

A migrated case in `crates/dimcode/tests/cli_flow*` runs only here: it is
`#[ignore]`d, and `cargo test -p dimcode --test cli_flow --test cli_tui_flow --test cli_session_flow --test cli_daemon_flow -- --ignored` runs it
locally beside the production flows. A case without a scenario stays selected by
CI.

Prerequisites:

| Requirement | Notes |
| --- | --- |
| `target/debug/dimcode` | Built from the checkout; `--binary` names another build |
| `target/debug/dim-app-server` | The provider scenario's `--server` profile; `--binary` looks for it beside the CLI, and the case names the build command when it is missing |
| tmux | 3.2 or newer, on macOS or Linux. `pane_dead_signal` arrived in 3.3: on 3.2 the field is unknown, so a signal death reports an empty status and fails the case instead of naming the signal. tmux 3.6a exits at `new-session -x -y` when `window-size manual` is in the startup config, so the harness sets it per window after `new-session` (#1890) |
| Node.js | The version in `.node-version`; the suite and its fixture are plain ES modules |
| No network | The model fixture listens on loopback; the suite sets `DIMCODE_UPDATE_CHECK=0` |

The suite is Unix-only because tmux is. Windows keeps its native boundary checks
(`cargo test --locked -p dim-cli --lib` in the `windows-cli` job) and the
component-level terminal tests that do not need tmux.

## What a case runs

Each case gets a disposable state of its own, under a short path in the system
temporary directory (`/tmp/dimtui-<run>/<case>`), because a unix socket name is
bounded:

| Piece | Detail |
| --- | --- |
| Product home | `--home <case>/home` with `rust/` at mode 0700; the product home `~/.dimcode` is never touched |
| User home | `HOME=<case>/user-home` and `TMPDIR=<case>/tmp`, so terminal helpers and tools write nothing outside the case |
| tmux server | One per case, on `<case>/tmux.sock`; `remain-on-exit` keeps the pane's last screen and its exit status. The server starts with the case's global environment (`HOME`, `TMPDIR`, `PATH`, `SHELL`) and `default-shell` pinned to `/bin/sh`, so no developer login shell or profile runs. `new-session` starts an inert `/bin/sh` placeholder, its window gets `window-size manual` so `resize-window` sets exact sizes, the output tap is attached, and only then is the CLI respawned into the pane, so no first frame escapes the tap |
| Daemon | The production App Server. Most cases run the CLI's own `app-server` subcommand with `--model-config`; the provider case runs the standalone `dim-app-server` next to the CLI (`--server`, no `--model-config`), so `/connect` manages real providers in the case's home |
| Model | A loopback OpenAI-compatible fixture; prompts map to fixed replies and scripted tool calls (`exec`, `ask_user`) |
| Environment | `TERM=screen-256color`, `LANG=en_US.UTF-8`, `SHELL=/bin/sh`, `HOME=<case>/user-home`, `TMPDIR=<case>/tmp`, `TMUX=<case socket>,0,0` so the CLI takes its multiplexer branch, `locale: en` in `config.json` |

The launcher scenario runs `npm/bin/dimcode.mjs` with a copied platform package,
so the wrapper's own code path starts the same native binary.

Input is real terminal input: `tmux send-keys -l` for text, `send-keys` for keys
(`C-j`, `Escape`, `BSpace`, `Down`), `send-keys -H` for bracketed pastes, and
`tmux resize-window` for resizes. Assertions read the pane with
`capture-pane -p -J` (the visible screen) and `capture-pane -p -J -S -` (the
scrollback above it). Waits are for text the interface draws. A resize waits for
the product's own reflow instead of a timer: the raw stream must show `CSI 3J`
after the resize and a completed frame (`CSI ?25h`), tmux must report the target
size, and a caller can name content the new frame must contain, so a clipped
frame from the previous width is never read. A bounded settle appears only
where the evidence is an absence: a key that must not send (`settleWithout`, 1.5
s of no request).

## Determinism and teardown

The fixture model is the only non-production component. Every scenario scripts
its replies, so a prompt has one expected answer; the tool calls a scenario needs
(`exec`, `ask_user`) are scripted per request. Requests are recorded with their
messages, so a scenario can assert what reached the model.

Teardown always runs: the tmux server is killed (ending the pane's CLI), the
daemon is stopped through `dimcode --home … daemon stop`, and every process
still naming the case's home is killed — the linked `dimcode-server` and the
standalone `dim-app-server` profile alike — then the fixture server is closed
and the case directory is removed. A scenario the runner's budget abandoned can
no longer issue tmux commands into a case that is closing: the case is marked
closed first and whatever the abandoned scenario throws is dropped. `SIGINT`,
`SIGTERM`, an unhandled rejection and an uncaught exception close the case in
progress through the same path before the runner exits, and the per-case close
runs in a `finally`, so a failing teardown cannot strand the run root or the
daemon. `--keep` leaves the home and raw streams in place for inspection.

## Scenarios

| Area | Scenario | Verifies |
| --- | --- | --- |
| terminal-modes | `scrollback_streams_and_exits_cleanly` | The default scrollback mode, streamed answer, committed history, `/exit`, `Bye!`, exit 0, no alternate screen, cursor restored, OSC 7501 working/clear |
| terminal-modes | `fullscreen_switches_screens_and_restores_the_terminal` | Fullscreen takes the alternate screen; `/tui` leaves and re-enters it; the multiplexer's `ModifyOtherKeys` enable/reset pair appears and balances; the last screen switch is a leave, so the final exit cannot lean on the first `/tui` |
| terminal-modes | `fullscreen_tool_body_click_collapses_only_its_node` | Fullscreen opens a tool batch and `exec` call via SGR mouse clicks, verifies drag, double-click, bracketed paste, and a near-deadline resize do not collapse it, and verifies clicking an output row in a scrolled 92×14 pane collapses only that call while re-anchoring its header and the reply below in view |
| terminal-modes | `resize_rewraps_history_without_losing_rows` | Resizing to 60x18 and 140x40 keeps the message and answer, rewraps rows, and draws to the new width: the capture is taken only after the product's own `CSI 3J` reflow and completed frame, tmux reports the target pane size, and the new frame is stable at that width |
| terminal-modes | `resize_during_a_stream_rewraps_and_finishes` | A resize while the fixture streams repaints the live output at the new width, keeps the streamed text, and the run still finishes |
| terminal-modes | `resize_during_thinking_rewraps_multiline_history` | Expanded reasoning streams in a turn sent as three lines; five resizes (100x24, 60x24, 120x40, 45x18, 100x36) keep every user line and every reasoning line, the transcript settles as a thought, and the answer still arrives |
| composer | `composer_edits_multiline_and_stores_a_large_paste` | `Ctrl+J` newline editing, backspace, a five-line bracketed paste stored as an attachment, and the exact prompt text the model receives |
| composer | `file_mention_inserts_a_workspace_link` | The `@` picker lists the `--cwd` workspace, inserts the chosen file, and completes a directory prefix to the file under it; the prompt carries both as absolute links |
| no-session | `home_commands_without_a_session_answer_and_send_nothing` | `/export`, `/queue` and `/mcp list` run from the palette over a draft answer at home in both layouts, the draft returns to the composer after each, no command sends it, and the fixture model receives no request; `/agent` and a valued `/effort` stay with `tui_home_commands_without_a_session_answer_and_send_nothing` in CI |
| hooks | `hook_context_and_stop_follow_up_stay_status_rows` | A prompt hook's context draws one closed row, named in the singular for its one line, and reaches the model after the operator's words; a stop hook's follow-up draws its own closed row and starts a turn; neither body nor the `<dim-hooks>` marker reaches the transcript; three `/tools` toggles open and close both |
| commands | `slash_palette_lists_curated_commands_and_settings` | The palette's curated order and filter, the Settings panel, and `/keys` output |
| commands | `palette_tab_completes_and_enter_runs_the_command` | `Tab` completes the command without running it, three `Tab`s keep completing and never exit the palette, and `Enter` still runs it, in both layouts |
| runs | `streaming_status_reports_and_interrupts` | The status row says `working…`/`esc interrupt`, OSC 7501 reports `working`, `Escape` interrupts with the recorded reason, and the status settles; with `DIM_TERMINAL_PROGRESS=on`, OSC 9;4 draws the working bar and takes it down when the run settles, and the last sequence the pane receives after exit is that clear |
| runs | `composer_borders_match_in_scrollback_and_fullscreen` | A slow-thinking turn in both scrollback and fullscreen at 100 and 60 columns places the run status, `esc interrupt`, and context usage together on the composer's bottom border, keeps the model on the top border (beside `! shell mode` in scrollback), and leaves only the permission preset on the footer line |
| input-queue | `queue_holds_steers_and_deletes` | Held messages in queue mode, the `Queue (n)` strip, `/queue`, `x` deletion read from the panel's own rows, `Enter` steer, the applied row in the transcript, and the deleted input never entering it |
| approvals | `approval_panel_allows_one_command` | The permission panel names the tool and command, `y` allows once, the command output returns to the model, and the status is blocked before the answer |
| approvals | `question_panel_answer_reaches_the_next_request` | The question panel's options, the review step, and the chosen label in the tool result the model receives |
| tasks | `background_task_row_and_tasks_panel` | A background command's task row, `1 in background`, the Background tasks panel, and stopping it with `x` |
| tasks | `task_strip_stays_and_switches_the_open_detail` | Two background commands keep their `◐` rows under an open task detail: in fullscreen a row click opens live output with `Esc close`, a second row click switches the detail, and Esc returns to the composer with its draft; in scrollback `/tasks` → Enter keeps the strip and `Esc back` |
| members | `member_conversation_and_stop_all` | A delegated member's task row, the member conversation's heading and transcript, `Ctrl+X` stop-all, and the leader's settled turn; read-only input and the leader draft stay with `tui_member_conversation_is_read_only_and_preserves_the_leader_draft` in CI |
| providers | `provider_panel_adds_and_keeps_a_custom_provider` | On the production App Server profile (`--server`, no `--model-config`) `/connect` adds a custom provider against the fixture, tests and applies it, a turn uses its key, and the product home keeps it across a daemon restart |
| providers | `chatgpt_plan_opens_at_connect` | In a fresh product home, `/connect` opens the ChatGPT consent panel and omits the legacy Codex entry |
| suggestions | `prompt_suggestion_offers_and_takes_the_next_message` | Every completed turn draws its own numbered offer; `Tab` takes it in scrollback, `Tab` keeps the focus in fullscreen (and `Enter` sends nothing) while `Right` takes it there, and `Escape` withdraws it without asking again |
| suggestions | `suggestion_asks_nothing_while_a_draft_exists` | A draft withdraws the offer, no suggestion request follows it, and the next completed turn asks again |
| daemon | `daemon_stop_reports_disconnect_and_resume_replays_history` | A stopped daemon keeps the draft in the same process and shows it, names `dimcode --resume`, exits cleanly with the terminal restored; the resumed session replays history and then answers a distinct new prompt, which only a new turn can produce (the draft is not restored across processes) |
| daemon | `daemon_build_mismatch_notice_scrollback` | A daemon stamped as another build of the same version is kept: scrollback shows the notice and the exact command that stops it, once on Home and once on a direct resume, and a client of the daemon's own build gets no notice |
| daemon | `daemon_build_mismatch_notice_fullscreen` | The same other-build notice on Home and on a direct resume in fullscreen, and no notice for an explicit `--server`, whose separate artifact is expected to carry another stamp |
| launcher | `npm_launcher_streams_and_passes_interrupts` | The npm launcher copies the native binary into its platform package, starts the TUI, streams an answer, shows the interrupt confirmation, and a second `Ctrl+C` exits 130 with the terminal restored |

## Effects pane text cannot show

The tmux server taps each pane's output stream with `pipe-pane`, writing it to
`<case>/<session>.raw`. A scenario reads that stream for the escapes a captured
screen cannot show:

| Effect | Evidence in the raw stream |
| --- | --- |
| Alternate-screen entry and exit | `CSI ? 1049 h` before the fullscreen composer; the last screen switch is a `CSI ? 1049 l`, so the first `/tui` leave cannot satisfy it |
| Cursor restoration | `CSI ? 25 h` in the final restore sequence, the part that starts at the status record's clear and runs to the process's last byte, so no frame or earlier mode switch can satisfy it |
| Keyboard protocol | The multiplexer branch tmux selects: `CSI > 4 ; 2 m` enables `ModifyOtherKeys` and `CSI > 4 ; 0 m` resets it, both counted and balanced. tmux answers the kitty probe without negotiating it, so `CSI > 5 u` never appears; actual kitty negotiation and the per-screen flag stacks stay with `exit::keyboard_protocol_follows_the_screen_and_restores_both_stacks_on_exit` in CI |
| Program Status Protocol | `OSC 7501` records: `state=idle`, `state=working`, `state=blocked:kind=permission`, and `state=clear` on exit |
| Terminal progress | `OSC 9;4` with `DIM_TERMINAL_PROGRESS=on` (the bar is suppressed inside a multiplexer by default): `9;4;3` while the run works, `9;4;0` once the run settles, and the stream's last `OSC 9;4` is `9;4;0` after exit. That last sequence cannot tell a settle from a repeated clear at exit, so a separate clear on `/exit` is `tui_reports_the_run_state_to_the_terminal_and_clears_it_on_exit` in CI (it waits for a second removal after `/exit`) |
| Process exit status and signals | `pane_dead_status` and `pane_dead_signal` from `remain-on-exit`: a clean exit is status 0 with no signal, the second interrupt is 130, and a signal death or an empty status fails the case instead of reading as 0. From tmux 3.3 the signal is also named; on 3.2 the field is unknown, and the failure is the empty status |

Inside tmux the OSC 7501 records stop at tmux and are not forwarded to the outer
terminal; the app writes them to the pane, so the tap sees them while the outer
terminal does not. Terminal-negotiation behavior that tmux cannot exhibit — the
startup color/DA probe, kitty flag stacks on a negotiating terminal, and the
theme choice from the terminal background — stays with the `dim-cli` component
tests in `crates/dim-cli/src/ui/terminal/` and the
`tui_default_theme_respects_background_and_saved_choice` case, which stays in
CI (#1810).

## Coverage mapping

| Required behavior | tmux scenario | Also asserted by |
| --- | --- | --- |
| Scrollback and fullscreen | `scrollback_streams_and_exits_cleanly`, `fullscreen_switches_screens_and_restores_the_terminal` | `cli_flow/exit.rs`, `dim-cli` render tests (CI) |
| Mode switching | `fullscreen_switches_screens_and_restores_the_terminal` | `exit::keyboard_protocol_follows_the_screen_and_restores_both_stacks_on_exit` (CI: negotiates kitty and restores each screen's flag stack) |
| Resizing | `resize_rewraps_history_without_losing_rows`, `resize_during_a_stream_rewraps_and_finishes`, `resize_during_thinking_rewraps_multiline_history` | `tui_resizing_during_thinking_preserves_multiline_history` (moved) |
| History and resume | `daemon_stop_reports_disconnect_and_resume_replays_history` | `daemon_replace::*` (CI) |
| Normal exit and interruption | `scrollback_streams_and_exits_cleanly`, `npm_launcher_streams_and_passes_interrupts`, `streaming_status_reports_and_interrupts` | `exit::ctrl_c_*` (CI, admitted: backpressure has no tmux equivalent) |
| Composer editing and paste | `composer_edits_multiline_and_stores_a_large_paste` | `interactive_composer_preserves_multiline_pastes_and_edits_atomic_markers` (CI) |
| File mentions and attachments | `file_mention_inserts_a_workspace_link`, `composer_edits_multiline_and_stores_a_large_paste` | `tui_at_mentions_pick_files_from_the_working_directory` (moved), paste flows (CI) |
| Slash commands and panels | `slash_palette_lists_curated_commands_and_settings`, `palette_tab_completes_and_enter_runs_the_command` | `tui_palette_tab_only_completes_and_enter_still_runs` (moved); palette and panel flows in `session_management`, `skills`, `workflow`, `mcp` (CI) |
| Missing session | `home_commands_without_a_session_answer_and_send_nothing` | `tui_mcp_without_a_session_reports_the_catalog_only` (CI: the scenario sees the answer but not that the daemon stays quiet); `tui_home_commands_without_a_session_answer_and_send_nothing` (CI: also runs `/agent` and a valued `/effort` without a model and keeps the daemon quiet); `tui_shell_session_creation_failure_…` (CI) |
| Hook rows | `hook_context_and_stop_follow_up_stay_status_rows` | `tui_draws_a_prompt_hooks_context_as_a_status_row` (moved); `tui_draws_a_stop_hooks_follow_up_as_one_row` (CI: the resumed `resumed session · N messages` count it is skipped by needs a resumed transcript) and `tui_draws_an_operators_own_hook_marker_verbatim` (CI) |
| Input suggestions | `prompt_suggestion_offers_and_takes_the_next_message`, `suggestion_asks_nothing_while_a_draft_exists` | four offer cases (moved); `enter_on_an_empty_composer_leaves_the_offer` (CI: the scrollback `Enter` on an empty composer); `a_narrow_terminal_…`, `settings_…` and the pending-question/goal suppression (CI) |
| Streaming and status | `streaming_status_reports_and_interrupts`, `scrollback_streams_and_exits_cleanly` | `tui_reports_the_run_state_to_the_terminal_and_clears_it_on_exit` (CI: also holds the paused OSC 9;4 `4;100` bar across a permission request and a question in both interface modes, and with the bar disabled); the scenario turns `DIM_TERMINAL_PROGRESS` on and additionally asserts OSC 9;4 `working` and its removal |
| Queue controls | `queue_holds_steers_and_deletes` | `tui_queue_panel_deletes_steers_and_moves_applied_inputs_to_the_transcript` (moved); recall and rapid controls (CI) |
| Permissions and questions | `approval_panel_allows_one_command`, `question_panel_answer_reaches_the_next_request` | `question::native_tui_question_answer_reaches_the_next_model_request` (moved); the panel cases (CI) |
| Task and subagent controls | `background_task_row_and_tasks_panel`, `member_conversation_and_stop_all` | `tui_tasks_panel_stops_the_selected_command_or_member` (CI: the scenario stops the selected command, this case also selects a member row and stops it); `tasks::native_tui_stop_all_…` (moved); member cards (CI) |
| Provider and account panels | `provider_panel_adds_and_keeps_a_custom_provider` | `tui_provider_management_uses_the_production_daemon_and_survives_restart` (moved); the account and model panels (CI) |
| Daemon disconnect/restart | `daemon_stop_reports_disconnect_and_resume_replays_history` | `tui_stays_up_when_the_daemon_leaves_and_restores_the_terminal_on_exit` (CI: the interface staying up, raw mode and a resize while disconnected need a daemon that leaves without shutting down); `daemon_replace::*` (CI) |
| Embedded daemon of another build | `daemon_build_mismatch_notice_scrollback`, `daemon_build_mismatch_notice_fullscreen` | `daemon_replace::build_identity::*` (CI: reuse for each stamp case, the pasted stop command with shell metacharacters and a launch-only `DIMCODE_HOME`, and an explicit `--server` outside the notice) |
| Production launcher and App Server | `npm_launcher_streams_and_passes_interrupts`; every scenario runs the production daemon | `unix_npm_wrapper_…` (moved); the 12 production flows the CI `--ignored` step names |

## Per-gap outcome

Every retained behavior whose case is now ignored has a replacement that proves
the same terminal behavior. Everything else stays in CI until #1810 gives it
one.

| Retained behavior | Outcome |
| --- | --- |
| Resize during thinking | **Moved**: `resize_during_thinking_rewraps_multiline_history` streams expanded reasoning into a three-line prompt, resizes through five sizes while it runs, and asserts every user line and reasoning line survives each repaint before the answer settles |
| Suggestion request suppression by a draft | **Moved**: `suggestion_asks_nothing_while_a_draft_exists` asserts that a draft withdraws the offer, no request follows, and the next turn asks again |
| Suggestion suppression by a pending question or an active goal, a daemon without the method, the narrow terminal and the settings row | **Admitted in CI** (#1810): no scenario proves them yet |
| Backpressure on exit (a reader that stops or a full output queue) | **Admitted in CI** (#1810): tmux always reads its pane, so the suite cannot exhibit it |
| Queue recall and rapid controls | **Admitted in CI** (#1810) |
| Resume catalog paging and its latency cases | **Admitted in CI** (#1810): they need a daemon that answers slowly |
| One-line hook singular wording, the operator's own hook marker | **Admitted in CI** (#1810) |
| The selected member row's stop (`x` on a member row), the scrollback `Enter` on an empty composer, and a follow-up's exclusion from the resumed `resumed session · N messages` count | **Kept in CI** (#1810): each is a distinct interaction the shared scenarios do not perform, so `tui_tasks_panel_stops_the_selected_command_or_member`, `enter_on_an_empty_composer_leaves_the_offer` and `tui_draws_a_stop_hooks_follow_up_as_one_row` stay selected |
| A daemon that leaves without shutting down (the interface stays up, keeps raw mode and reflows a resize while disconnected) | **Kept in CI** (#1810): the suite's daemon is the CLI's own App Server, so stopping it ends the interface through the disconnect path; `tui_stays_up_when_the_daemon_leaves_and_restores_the_terminal_on_exit` needs a daemon that only closes the connection |
| Account sign-in, model picker and management, MCP/skills/hooks/scheduled/workflow panels, goal and context rows, title, windows, startup model adoption, member cards and approval | **Admitted in CI** (#1810) |
| UI-leaf ownership, close and terminal restoration (`tests/ui_leaf.rs`) | **Admitted in CI** (#1810) |

## Dispositions from #1802

#1802 reviewed the TUI PTY flows against `dim-cli`'s component coverage and
recorded a verdict per case; #1805 (merged as `375b4db3`) applied the merges.
The verdicts below are the migration input; the inventory marks each case with
the verdict and where its behavior lives afterwards.

| Area | Verdicts |
| --- | --- |
| Exit under backpressure | `ctrl_c_ends_the_interface_when_the_terminal_stops_reading_after_the_goodbye_panel`, `ctrl_c_ends_the_interface_when_the_output_queue_is_already_full` and `keyboard_protocol_follows_the_screen_and_restores_both_stacks_on_exit` are kept as stream-level assertions |
| Queue controls | `tui_queue_panel_deletes_steers_and_moves_applied_inputs_to_the_transcript`, `tui_rapid_queue_controls_settle_each_held_input_once` and `tui_queue_recall_refuses_attachments_and_steering_preserves_all_content` are kept; `tui_queue_shows_held_messages_and_steers_or_withdraws_them` is rewritten (Ctrl+S steer, Shift+← recall, applied message, child-branch stream) |
| Context and compaction | Five cases kept: composer usage, threshold warning, compaction row, `/compact` outcomes, and the resumed compaction row |
| Hook transcript rows | `tui_draws_a_prompt_hooks_context_as_a_status_row` is kept and absorbs `tui_names_a_one_line_hook_context_in_the_singular`; `tui_draws_a_stop_hooks_follow_up_as_one_row` and `tui_draws_an_operators_own_hook_marker_verbatim` are kept |
| Model picker | Four cases kept: localized hint, no-management guidance, empty providers, and on-demand paging |
| Input suggestions | `right_takes_the_offer_into_the_draft_without_sending`, `a_narrow_terminal_truncates_the_offer` and the ten request/suppression cases are kept; `tab_takes_the_offer_in_scrollback` merges into `tab_keeps_the_fullscreen_focus_and_right_still_takes_the_offer`, which is kept |
| Missing session | `/export`, `/queue`, `/agent` and a valued `/effort` merge into the new `tui_home_commands_without_a_session_answer_and_send_nothing`; `tui_shell_session_creation_failure_settles_with_the_no_session_notice` and `tui_mcp_without_a_session_reports_the_catalog_only` are kept |
| Coverage to account for | `tui_resizing_during_thinking_preserves_multiline_history`, `unix_npm_wrapper_preserves_the_native_terminal_and_interrupt_flow`, `tui_provider_management_uses_the_production_daemon_and_survives_restart`, `question::native_tui_question_answer_reaches_the_next_model_request` and `tasks::native_tui_stop_all_settles_the_member_and_reports_cancellation_to_the_leader` were kept by #1802 and are each moved by the scenario named in the inventory; `tui_stays_up_when_the_daemon_leaves_and_restores_the_terminal_on_exit` stays in CI because the suite cannot exhibit a daemon that leaves without shutting down |
| Startup routing carry-forward | `fresh_native_startup_uses_workspace_and_ask_without_policy_file` moves with the line-mode production flows, not the TUI suite; see [Line-mode routing exception](#line-mode-routing-exception) |

The no-session gate and the input-suggestion flows are the groups this PR
moved: `home_commands_without_a_session_answer_and_send_nothing` runs each
refused command from the palette over a draft, and
`prompt_suggestion_offers_and_takes_the_next_message` covers the offers and
their keyboard actions. The rest of those groups — `/agent` and a valued
`/effort`, the scrollback `Enter` on an empty composer, the pending-question and
goal suppression and the request-suppression cases — stay selected by CI, as
does the resumed `resumed session · N messages` count a follow-up is skipped by.

## Existing terminal E2E cases

`crates/dimcode/tests/cli_flow*.rs` holds 305 cases that drive the TUI (or the
launcher that passes through to it) through a PTY, spread over 26 modules; the
inventory below lists each one with the disposition #1802 gave it. #1805 is
merged, so the five cases it merged are gone and its no-session survivor is in
their place. Of those 305, 10 are now `#[ignore]`d with the reason "TUI E2E runs
in the local tmux suite" because a tmux scenario proves their behavior; 8 were
already ignored for production (4 of them now have a scenario); the other 287
stay selected by CI until #1810 replaces them. `tests/ui_leaf.rs` adds 4 UI
leaf cases, all still selected by CI.

States (the #1802 disposition, not the current routing):

- **tmux `scenario`** — a scenario exercises the same user-visible flow end to
  end.
- **keep** — #1802 kept the case because its assertions still carry behavior.
- **rewrite** — #1802 kept the case with redundant assertions removed.
- **unreviewed** — #1802's scope did not reach it.
- **CI** — the case was selected by CI when #1802 reviewed it.

**`accounts`** — 3 cases

- unreviewed (#1802), CI — `dim_device_login_can_retry_the_model_refresh_from_the_account_panel`, `the_provider_panel_signs_in_through_each_described_namespace`, `a_panel_without_the_sign_in_method_shows_the_unsupported_daemon_error`

**`acp`** — 1 case

- unreviewed (#1802), CI — `tui_reports_max_tokens_action_and_accepts_the_next_prompt`

**`catalog`** — 18 cases

- unreviewed (#1802), CI — `tui_resume_catalog_pages_searches_and_closes_without_waiting_for_a_read`, `tui_resume_labels_origins_and_shows_acp_sessions_when_asked`, `tui_resume_ctrl_s_widens_to_every_workspace_and_names_the_workspace_of_a_row`, `tui_resume_says_when_the_workspace_has_no_saved_sessions`, `tui_resume_hides_other_workspaces_on_the_summaries_fallback`, `a_cold_ui_leaf_submits_its_first_message_with_a_large_native_catalog` (ignored, production), `tui_resume_toggles_between_the_repository_and_all_workspaces`, `tui_resume_keeps_the_repository_rows_on_the_summaries_fallback`, `tui_resume_names_a_one_worktree_repository`, `tui_resume_draws_the_workspace_while_a_slow_git_runs`, `tui_resume_skips_the_repository_scope_a_daemon_cannot_read`, `tui_resume_ctrl_x_deletes_the_selected_row_and_keeps_the_shown_session`, `tui_resume_esc_leaves_the_picker_while_the_delete_is_in_flight`, `tui_resume_deletes_the_shown_session_and_returns_home`, `tui_resume_delete_keeps_the_row_when_the_daemon_cannot_delete`, `tui_resume_delete_keeps_the_row_on_a_failure_and_refreshes_when_nothing_removed`, `tui_resume_delete_after_the_close_lands_home`, `the_resume_hint_is_drawn_translated`

**`context`** — 12 cases

- unreviewed (#1802), CI — `tui_display_details_use_one_typed_reply_and_retry_after_collapse`, `tui_displays_history_and_handles_input_while_context_usage_is_pending`, `tui_discards_a_previous_sessions_delayed_context_report`, `tui_compaction_reports_committed_failures_once_and_keeps_call_errors`, `tui_shows_context_usage_the_hello_does_not_list`, `tui_shows_no_reading_and_keeps_working_when_context_usage_fails`, `tui_usage_shows_the_last_requests_cache_hit_rate`, `tui_usage_rounds_the_cache_hit_rate_down_and_hides_an_impossible_usage`, `tui_usage_shows_the_cache_hit_rate_a_resumed_session_recorded`, `tui_usage_cache_hit_rate_ignores_events_off_main`, `tui_usage_withholds_the_cache_hit_rate_until_main_history_loads`, `tui_context_reading_follows_each_finished_model_request_on_main`

**`exit`** — 3 cases

- keep (#1802), CI — `keyboard_protocol_follows_the_screen_and_restores_both_stacks_on_exit`, `ctrl_c_ends_the_interface_when_the_terminal_stops_reading_after_the_goodbye_panel`, `ctrl_c_ends_the_interface_when_the_output_queue_is_already_full`

**`goal`** — 4 cases

- unreviewed (#1802), CI — `tui_goal_shows_the_state_and_marks_the_status_area`, `tui_goal_acts_on_the_shown_line_and_a_new_line_has_none`, `tui_goal_reports_a_failed_read_and_sends_nothing`, `tui_goal_reads_owner_state_while_the_transcript_window_is_loading`

**`hooks`** — 20 cases

- unreviewed (#1802), CI — `hooks_toggle_conflict_reads_the_area_again`, `hooks_edit_writes_what_the_editor_leaves`, `hooks_edit_writes_nothing_when_the_editor_changes_nothing`, `hooks_edit_keeps_the_users_text_when_it_does_not_parse`, `hooks_edit_keeps_the_users_text_when_the_revision_is_stale`, `hooks_edit_keeps_one_file_per_edit`, `hooks_edit_writes_nothing_when_the_editor_exits_non_zero`, `hooks_edit_runs_an_editor_path_with_a_space`, `tui_hooks_panel_lists_the_sources_and_toggles_the_project`, `a_missing_source_shows_its_path_and_says_there_is_no_hooks_file`, `tui_hooks_panel_edits_the_user_document`, `tui_hooks_panel_offers_no_edit_without_a_user_document`, `hooks_edit_and_set_refuse_a_daemon_without_a_user_document`, `tui_hooks_panel_settles_a_write_whose_read_fails`, `tui_hooks_panel_drops_the_draft_when_the_conflict_read_fails`, `tui_hooks_panel_keeps_the_users_text_when_the_write_is_refused`, `tui_hooks_panel_reopens_the_users_text_after_a_refusal`, `tui_hooks_panel_writes_nothing_when_the_editor_exits_non_zero`, `hooks_commands_refuse_a_daemon_that_does_not_advertise_them`, `tui_hooks_panel_says_when_the_daemon_cannot_manage_them`

**`mcp`** — 17 cases

- unreviewed (#1802), CI — `resumed_mcp_line_commands_use_the_session_workspace_in_both_clients`, `tui_mcp_panel_lists_manages_and_tests_entries`, `tui_mcp_panel_edits_and_creates_entries`, `tui_mcp_panel_seeds_a_remote_patch_without_stored_values`, `tui_mcp_panel_shows_one_entries_failed_configuration_read`, `tui_mcp_panel_lists_entries_when_the_read_is_absent`, `tui_mcp_panel_keeps_the_users_text_when_the_revision_is_stale`, `tui_mcp_panel_reports_a_refused_write`, `tui_mcp_panel_draws_tool_text_sanitized`, `tui_mcp_panel_shows_a_failed_catalog_read`, `mcp_edit_opens_the_patch_without_stored_values`, `tui_mcp_list_keeps_a_session_state_on_one_line`, `mcp_disconnect_closes_the_entry_connections`, `tui_mcp_panel_keeps_the_users_text_when_a_create_finds_a_conflict`, `tui_mcp_panel_marks_a_broken_entry_invalid_and_offers_removal`, `tui_mcp_panel_removes_the_row_of_the_source_it_names`, `tui_mcp_line_commands_take_the_source_that_picks_the_entry`

**`models`** — 9 cases

- unreviewed (#1802), CI — `models_reports_account_discovery_failure_instead_of_an_empty_list`, `tui_connect_disabled_provider_refreshes_its_model_management_panel`, `tui_models_area_keeps_a_refresh_whose_read_fails`, `picker_and_limit_declaration_read_the_same_catalog`, `tui_models_area_opens_and_toggles_a_model`, `tui_models_area_offers_no_toggle_for_an_unavailable_model`, `tui_models_area_keeps_the_panel_on_a_refused_write`, `tui_models_area_rereads_after_a_conflict`, `tui_models_area_reports_a_conflict_whose_reread_fails`

**`permission`** — 9 cases

- unreviewed (#1802), CI — `tui_resume_requires_the_explicit_permission_before_attaching`, `tui_permission_flag_overrides_saved_controls_and_invalid_defaults_are_ignored`, `tui_home_remembers_permissions_without_login_and_applies_them_to_the_first_message`, `tui_permissions_panel_lists_command_rules_and_returns`, `tui_permissions_panel_adds_and_removes_command_rules`, `tui_permissions_panel_opens_a_placeholder_template`, `tui_permissions_panel_reports_a_raced_remove`, `tui_permissions_panel_shows_a_rule_written_elsewhere_on_the_next_read`, `tui_permissions_arguments_open_the_panel_and_are_not_ignored`

**`prompt_suggestion`** — 13 cases

- merged → `prompt_suggestion::tab_keeps_the_fullscreen_focus_and_right_still_takes_the_offer` (#1805, gone) — `tab_takes_the_offer_in_scrollback`
- tmux `prompt_suggestion_offers_and_takes_the_next_message` — keep — `a_completed_turn_asks_for_the_next_message_and_draws_the_offer`, `right_takes_the_offer_into_the_draft_without_sending`, `tab_keeps_the_fullscreen_focus_and_right_still_takes_the_offer`, `escape_and_an_edit_withdraw_the_offer`
- tmux `suggestion_asks_nothing_while_a_draft_exists` — keep — `a_draft_keeps_the_interface_from_asking`
- keep (#1802), CI — `a_daemon_that_does_not_list_the_method_is_never_asked`, `a_pending_question_keeps_the_interface_from_asking`, `an_active_goal_keeps_the_interface_from_asking`, `enter_on_an_empty_composer_leaves_the_offer` (scrollback `Enter` on an empty composer is not in a scenario), `a_narrow_terminal_truncates_the_offer`, `the_pause_is_announced_once_per_session`, `settings_shows_no_suggestion_row_without_the_methods`, `settings_toggles_the_switch_through_the_daemon`

**`prompts`** — 1 case

- unreviewed (#1802), CI — `prompt_edit_writes_what_the_editor_leaves`

**`protocol_version`** — 2 cases

- unreviewed (#1802), CI — `a_resumed_session_of_a_newer_protocol_names_both_versions_and_offers_no_retry`, `a_resumed_session_of_an_older_protocol_opens` (ignored, production)

**`question`** — 8 cases

- tmux `question_panel_answer_reaches_the_next_request` — keep — `native_tui_question_answer_reaches_the_next_model_request` (ignored, production boundary)
- unreviewed (#1802), CI — `tui_questions_keep_structured_answers_and_the_leader_draft`, `tui_questions_retry_and_keep_call_identity_across_late_replies`, `tui_questions_restore_a_child_and_interrupt_its_branch`, `tui_member_questions_stay_read_only_and_a_leader_question_takes_the_prompt`, `tui_commands_run_over_a_pending_question`, `tui_command_arguments_run_over_a_pending_question`, `tui_free_text_answers_keep_their_slashes_and_the_palette_survives_a_dropped_question`

**`scheduled`** — 14 cases

- unreviewed (#1802), CI — `user_queue_reports_a_daemon_without_block_presence`, `automatic_inputs_stay_out_of_the_user_queue_after_resume_and_live_delivery`, `scheduled_edit_writes_what_the_editor_leaves`, `tui_scheduled_panel_lists_and_manages_jobs`, `tui_scheduled_panel_edits_and_creates_jobs`, `tui_scheduled_panel_keeps_the_users_text_when_the_revision_is_stale`, `tui_scheduled_panel_reports_a_conflict_on_remove`, `tui_scheduled_panel_shows_every_key_and_confirmation_at_80_columns`, `tui_scheduled_panel_keeps_the_users_text_when_a_create_is_refused`, `tui_scheduled_panel_keeps_a_refused_edit_for_its_own_job`, `scheduled_draws_a_job_name_on_one_line`, `tui_scheduled_panel_reports_an_unreadable_create_reply_as_applied`, `tui_scheduled_panel_says_when_the_daemon_cannot_manage_it`, `a_held_scheduled_input_is_left_by_send_now_and_by_escape_then_enter`

**`session_management`** — 7 cases

- unreviewed (#1802), CI — `tui_rename_sets_and_clears_the_title_without_submitting_it`, `tui_palette_rename_waits_for_the_title`, `tui_rename_alone_is_refused_with_the_needs_notice`, `tui_rename_during_a_running_turn_is_a_command_not_a_queued_input`, `tui_rename_without_a_session_sends_nothing`, `tui_rename_reports_a_daemon_without_the_method`, `session_cleanup_asks_on_a_terminal`

**`skills`** — 5 cases

- unreviewed (#1802), CI — `skill_discovery_reports_failures_and_a_failed_reload_keeps_the_palette`, `tui_skills_panel_reads_again_after_a_failed_read`, `tui_skills_panel_lists_toggles_and_reloads`, `tui_skills_panel_reloads_an_empty_catalog`, `tui_skills_panel_says_when_the_daemon_cannot_manage_them`

**`staged_update`** — 6 cases

- unreviewed (#1802), CI — `tui_update::update_now_stops_an_idle_daemon_and_restarts_on_the_same_session`, `tui_update::update_now_refuses_a_busy_daemon_and_prints_manual_command`, `tui_update::update_now_falls_back_to_manual_command_for_an_old_daemon`, `tui_update::update_now_cancels_cleanly_on_sigint_while_stopping_daemon`, `tui_update::update_now_waits_for_a_released_history_lock_before_restarting`, `tui_update::update_now_restarts_after_a_bounded_wait_when_history_lock_stays_held`

The interrupted-check case is line mode and stays in CI with the other `update`
flows.

**`startup`** — 23 cases

- unreviewed (#1802), CI — `first_shell_command_creates_once_without_submitting_a_model_turn`, `home_previews_the_model_without_creating_a_session_and_survives_preview_errors`, `bare_effort_at_home_lists_the_preview_models_choices`, `bare_effort_at_home_without_a_model_says_what_to_do`, `a_choice_at_home_reaches_the_first_session_and_saves_nothing`, `a_model_picked_at_home_drops_the_effort_home_chose`, `home_that_inherits_a_model_refuses_a_preference_choice`, `fast_at_home_lists_tiers_and_holds_a_choice_for_the_first_session`, `first_text_or_draft_creates_once_with_startup_options_in_both_tui_modes`, `a_refused_leaf_start_shows_the_error_restores_the_terminal_and_exits`, `direct_startup_reaches_each_creation_entry_without_persisting_secrets`, `resume_startup_is_adopted_or_explicitly_rejected_before_input_in_every_client`, `home_first_message_inherits_the_adopted_model_without_replaying_startup`, `temporary_model_preferences_use_adopted_choices_without_a_provider_catalog`, `home_creation_uses_the_advertised_product_or_configured_model`, `a_first_prompt_is_submitted_once_and_the_interface_stays`, `a_first_prompt_submits_its_image_with_the_text`, `a_first_prompt_on_a_resumed_session_submits_into_it`, `a_first_prompt_that_names_a_command_runs_it`, `a_first_prompt_without_a_session_stays_in_the_draft`, `a_first_prompt_is_kept_when_the_resumed_session_cannot_attach`, `a_first_prompt_a_new_draft_keeps_out_is_reported`, `home_browsing_and_exit_need_no_session_in_both_tui_modes`

**`tasks`** — 33 cases

- keep (#1802), CI — `native_tui_stop_all_settles_the_member_and_reports_cancellation_to_the_leader` (ignored, production boundary)
- unreviewed (#1802), CI — `tui_shows_the_transcript_and_submits_input_before_owner_inventory_finishes`, `opening_a_discovered_line_loads_its_transcript_through_the_task_history_queue`, `tui_task_history_resume_windows_complete_main_pages_and_preserves_the_draft`, `tui_titled_silent_command_details_keep_the_command`, `tui_fullscreen_resize_cancels_a_task_release_before_reflow`, `tui_task_inventory_restores_branch_pages_and_retries_only_on_reopen`, `tui_task_inventory_discards_old_pages_and_queued_retries_after_resuming_the_same_session`, `tui_task_stop_all_keeps_its_finite_targets_and_waits_only_for_replies`, `tui_task_stop_all_excludes_delayed_discovery_and_settles_failed_branch_reads`, `tui_task_controls_queued_before_a_replacement_never_retarget_it`, `tui_task_stop_all_keeps_admitted_targets_after_a_session_switch`, `tui_task_end_restored_by_history_is_not_notified_but_a_live_one_is`, `tui_task_inventory_keeps_fifty_terminal_facts_and_their_original_results`, `tui_member_conversation_is_read_only_and_preserves_the_leader_draft`, `tui_member_conversation_settles_old_attach_or_page_before_observing_a_replacement`, `tui_member_conversation_keeps_errors_local_and_reopens_from_a_fresh_history`, `tui_member_conversation_yields_to_a_leader_approval_and_cleans_up_its_held_page`, `tui_main_history_failure_is_visible_and_disables_submission`, `tui_workflow_runs_are_discovered_from_facts_and_stopped_through_their_owner`, `tui_workflow_inventory_errors_block_stop_all_until_reopen_and_stops_follow_other_targets`, `tui_reused_temporary_agents_keep_their_names_after_a_public_directory_read`, `tui_member_cards_share_observations_and_show_the_members_actual_model`, `tui_member_cards_attach_only_unfinished_members_and_keep_a_failed_read_unavailable`, `tui_member_cards_read_agent_names_once_for_ids_the_list_does_not_return`, `tui_member_cards_name_a_native_agent_the_hello_does_not_announce`, `tui_earlier_member_cards_use_cached_and_newly_discovered_agent_names`, `tui_member_cards_keep_the_id_when_the_shared_agent_list_read_fails`, `tui_rejected_delegations_are_not_agents_the_leader_worked_with`, `tui_new_creates_the_session_on_the_first_message`, `tui_page_up_at_the_top_draws_the_branch_from_its_first_message`, `earlier_on_an_ended_line_keeps_the_oldest_loaded_message`, `tui_resume_restores_settled_delegations_outside_the_window_in_dispatch_order`

**`title`** — 8 cases

- unreviewed (#1802), CI — `title_notice_after_the_first_message_updates_the_window_title`, `resuming_a_titled_session_shows_its_title_and_never_its_first_message`, `a_cleared_title_falls_back_to_the_first_message`, `a_rename_from_another_client_updates_the_window_title`, `a_title_notice_for_a_session_the_client_left_changes_nothing`, `an_older_or_equal_title_revision_is_ignored`, `new_returns_the_window_title_to_the_product`, `stored_titles_are_trimmed_and_placeholders_are_no_title`

**`update_entry`** — 1 case

- unreviewed (#1802), CI — `update_reports_a_source_build_and_sends_no_input`

**`windows`** — 2 cases

- unreviewed (#1802), CI — `an_orphan_tool_result_reads_its_arguments_only_after_expanding_details`, `tui_attach_keeps_buffered_configuration_and_skips_ended_branch_inventories`

**`workflow`** — 13 cases

- unreviewed (#1802), CI — `workflow_edit_set_and_history_find_a_disabled_project_definition`, `tui_workflow_panel_lists_definitions`, `tui_workflow_save_command_writes_a_definition`, `tui_workflow_panel_starts_a_run_with_parameters`, `tui_workflow_panel_keeps_a_runs_parameters_for_its_own_definition`, `tui_workflow_run_leaves_a_listed_default_unsent`, `tui_workflow_panel_starts_a_run_on_the_shown_line`, `tui_workflow_starts_without_a_session_and_reports_nowhere`, `tui_workflow_panel_reports_a_conflict_and_rereads`, `tui_workflow_panel_refuses_a_rename_and_keeps_the_draft`, `tui_workflow_save_is_refused_when_the_daemon_cannot_write`, `tui_workflow_save_reports_an_invalid_definition_and_keeps_the_draft`, `tui_workflow_panel_offers_no_write_without_the_settings_methods`

**`worktrees`** — 1 case

- unreviewed (#1802), CI — `managed_worktree_tui_uses_native_ownership_across_restart_and_cleanup` (ignored, production)

**`cli_flow`** — 72 cases

- merged → `tui_home_commands_without_a_session_answer_and_send_nothing` (#1805, gone) — `tui_agent_without_a_session_answers_the_no_session_notice_and_sends_nothing`, `tui_valued_effort_at_home_without_a_model_says_what_to_do_and_sends_nothing`, `tui_export_and_queue_require_an_open_session`
- keep (#1802), CI — `tui_home_commands_without_a_session_answer_and_send_nothing`
- merged → `tui_draws_a_prompt_hooks_context_as_a_status_row` (#1805, gone) — `tui_names_a_one_line_hook_context_in_the_singular`
- tmux `resize_during_thinking_rewraps_multiline_history` — keep — `tui_resizing_during_thinking_preserves_multiline_history`
- tmux `file_mention_inserts_a_workspace_link` — unreviewed — `tui_at_mentions_pick_files_from_the_working_directory`
- tmux `npm_launcher_streams_and_passes_interrupts` — keep — `unix_npm_wrapper_preserves_the_native_terminal_and_interrupt_flow` (ignored, production boundary)
- tmux `queue_holds_steers_and_deletes` — keep — `tui_queue_panel_deletes_steers_and_moves_applied_inputs_to_the_transcript`
- tmux `background_task_row_and_tasks_panel` — keep, CI (the scenario stops the selected command; the selected member row stays with this case) — `tui_tasks_panel_stops_the_selected_command_or_member`
- tmux `hook_context_and_stop_follow_up_stay_status_rows` — keep — `tui_draws_a_prompt_hooks_context_as_a_status_row`
- keep, CI (the scenario draws the live follow-up row and its three `/tools` toggles; the resumed `resumed session · N messages` count it is skipped by stays with this case) — `tui_draws_a_stop_hooks_follow_up_as_one_row`
- tmux `streaming_status_reports_and_interrupts` — keep, CI (the scenario adds OSC 9;4 `working` and its clear; the paused bar, the clear after a permission request and a question, and the disabled configuration stay with this case) — `tui_reports_the_run_state_to_the_terminal_and_clears_it_on_exit`
- keep (#1802), CI — `tui_mcp_without_a_session_reports_the_catalog_only`, `tui_shell_session_creation_failure_settles_with_the_no_session_notice`, `tui_stays_up_when_the_daemon_leaves_and_restores_the_terminal_on_exit`, `tui_provider_management_uses_the_production_daemon_and_survives_restart` (ignored, production boundary), `tui_rapid_queue_controls_settle_each_held_input_once`, `tui_queue_recall_refuses_attachments_and_steering_preserves_all_content`, `tui_composer_shows_context_use_and_refreshes_it_when_a_run_ends`, `tui_compaction_row_runs_then_shows_the_summary_it_recorded`, `tui_compact_displays_committed_outcomes_and_reports_an_unsupported_daemon`, `tui_context_warning_starts_at_the_threshold_share_and_a_compaction_refreshes_it`, `tui_rebuilds_compaction_rows_from_history_on_resume`, `tui_draws_an_operators_own_hook_marker_verbatim`, `tui_models_picker_hides_manage_models_without_management_methods`, `tui_models_picker_draws_its_hint_in_the_interface_language`, `tui_models_picker_reads_pages_on_demand_and_reopens_without_a_cache`, `tui_models_picker_names_every_provider_with_no_selectable_model`
- rewrite (#1802), CI — `tui_queue_shows_held_messages_and_steers_or_withdraws_them`
- unreviewed (#1802), CI — `explicit_model_selection_uses_protocol_field_names_from_tui_and_line_client`, `fullscreen_session_attached_before_the_first_frame_shows_one_banner`, `the_model_label_shows_the_adopted_reasoning_effort_in_both_modes`, `a_new_session_shows_the_effort_its_creation_adopted`, `the_model_label_shows_a_configured_profile_effort` (ignored, production), `home_after_new_drops_the_previous_sessions_effort`, `repicking_the_current_model_keeps_the_effort`, `a_resumed_session_draws_the_adopted_model_and_effort`, `another_clients_adoption_redraws_the_model_label`, `a_narrow_composer_drops_the_effort_before_the_model`, `tui_escape_sends_the_reason_that_records_the_interruption`, `tui_shell_mode_runs_through_session_methods_and_shows_the_recorded_run`, `tui_effort_dialog_selects_the_current_models_effort_and_cancels_without_writing`, `tui_mcp_and_configuration_commands_use_session_methods`, `tui_pasted_images_and_long_text_submit_as_user_content`, `tui_skill_rows_come_from_the_catalog_and_complete_without_sending`, `reloading_skills_changes_the_palette_rows`, `a_daemon_without_skill_catalog_keeps_a_working_command_palette`, `tui_palette_tab_only_completes_and_enter_still_runs`, `tui_sends_dropped_files_as_links_and_images_by_model_capability`, `fullscreen_login_releases_mouse_for_the_link_and_restores_the_preference_on_close`, `tui_mcp_says_when_no_server_is_configured`, `tui_mcp_shows_failure_reasons_rejections_and_the_catalog_error`, `tui_mcp_enables_and_disables_a_project_server`, `tui_web_toggles_the_daemon_switch_without_leaving`, `tui_registers_with_the_ui_registry_and_leaves_when_asked`, `tui_export_dialog_matches_the_subcommand_and_cancel_sends_nothing`, `tui_ctrl_enter_interrupts_the_run_and_starts_a_new_one_with_the_draft`, `tui_ctrl_s_steers_the_draft_while_a_run_is_in_progress`, `tui_escape_withdraws_the_queue_before_the_run_stops`, `tui_command_output_refreshes_one_read_and_preserves_its_branch_and_terminal_result`, `tui_command_output_errors_stop_refresh_and_reopening_retries`, `tui_permissions_keep_pending_file_and_member_questions_and_save_command_rules`, `tui_approval_alias_selects_presets_and_remembers_them`, `tui_shift_tab_cycles_permissions_and_preserves_the_draft_in_both_modes`, `tui_yields_the_title_and_the_bar_to_a_terminal_that_keeps_the_record`, `cold_hosted_tui_opens_without_reading_the_session_catalog`, `resume_by_id_takes_its_model_from_the_attach_record`, `resume_by_id_finds_the_model_in_the_catalog_when_the_attach_reply_is_empty`, `tui_provider_create_recovery_preserves_changed_draft_until_confirmation`, `tui_models_picker_edits_only_the_subagent_model_policy`, `tui_models_picker_cycles_reasoning_effort`, `tui_models_picker_writes_the_default_the_launch_workspace_reads`, `member_tool_approval_requires_trusted_details_in_both_terminal_modes`, `member_approval_without_the_detail_method_offers_only_rejection`, `tui_default_theme_respects_background_and_saved_choice`

Modules: 26; cases: 305; already ignored before this PR: 8 (4 now covered by a
tmux scenario: `unix_npm_wrapper_…`, `tui_provider_management_…`,
`native_tui_question_answer_…`, `tasks::native_tui_stop_all_…`); newly ignored
for the tmux suite: 10; moved to the tmux suite: 14; still ignored with no
scenario: `a_cold_ui_leaf_…`, `a_resumed_session_of_an_older_protocol_opens`,
`managed_worktree_tui_…`, `the_model_label_shows_a_configured_profile_effort`;
selected by CI: 287 in the `cli_flow*` targets; the 4 `ui_leaf` cases run in
their own target.

The `cli_flow*` targets (`cli_flow`, `cli_tui_flow`, `cli_daemon_flow`,
`cli_session_flow`) also hold the line-mode, ACP, daemon and production flows
that CI keeps: 434 cases on Linux (432 on macOS), of which 422 run by default
(420 on macOS) and 12 are `--ignored` production flows in `cli_flow` that the
CI selection names one by one. The five cases #1805
merged, and the one case #1805 added, are recorded in the dispositions above
rather than listed here.


## Failure artifacts

A failed case copies everything needed to diagnose it to
`target/tui-tmux/<run>/<scenario>/` (or `--artifacts DIR`):

| File | Content |
| --- | --- |
| `failure.txt` | The failure and its stack |
| `<session>-screen.txt` | The visible screen at failure, joined by `-J` |
| `<session>-scrollback.txt` | The pane's history plus the visible screen |
| `<session>-pane.raw` | The raw output stream: escapes, OSC reports, draws |
| `requests.jsonl` | Every model request the fixture received, with its messages |
| `interactions.log` | Each send, key, paste, resize and command, with timestamps |
| `daemon.log`, `config.json`, `model.json` | The daemon's log and the case's configuration |
| `home-listing.txt` | The product home's files and sizes |

A passing run leaves nothing behind: the case directory, its home and its tmux
server are removed. `--keep` keeps them meanwhile and still writes the captures.

## CI

No CI job runs a **migrated** TUI flow. The 10 cases whose behavior a tmux
scenario proves are `#[ignore]`d with the reason "TUI E2E runs in the local tmux
suite", so the ordinary `cargo test --workspace` selection compiles them and
runs none of them; the production `--ignored` step names only the flows CI
keeps. The other 287 `cli_flow*` TUI cases and the 4 `ui_leaf` cases have no
replacement yet and stay selected by CI, because dropping them would remove
runtime coverage that only they assert (#1810). Terminal E2E coverage that has
moved is a local verification step: a display-less runner and the suite's
terminal interactions would add time without protecting a boundary that
component and protocol tests do not already cover.
`scripts/tui-tmux.test.mjs` fails if any workflow names the suite or if the
`cli_flow` ignored step goes back to selecting every ignored case.

CI keeps:

- Ordinary workspace tests: the `dim-cli` component render and keyboard tests,
  `dim-app-server` protocol and transport flows, the 422 non-TUI `cli_flow*`
  cases (`--no-tui`, `exec`, piped prompts, ACP, `staged_update::recovery::`,
  and the scripted daemon flows), and the 287 TUI cases #1810 still tracks
  (including the 6 `staged_update::tui_update::` cases, which use the debug-only
  `DIM_TEST_RELEASE_BUILD=1` override instead of a separate release-stamped
  build step).
- The explicit `cli_flow` step that names the 12 `--ignored` production and
  line-mode flows one by one: the production App Server, the account and
  permission presets, the real-owner MCP/hooks/workflow writes, and the
  line-mode startup case below.

### Line-mode routing exception

`fresh_native_startup_uses_workspace_and_ask_without_policy_file` (#1802:
**move**) is line mode, not a TUI case: it runs `mcp list` against the production
App Server and never opens the terminal UI. It stays selected in the ignored CLI
step by name, and it must not be dropped with the ignored TUI cases or moved
into the tmux suite.

Its unique assertions stay with it: a fresh home starts the production App Server
through `--server`, `mcp list` succeeds, the daemon's `settings.read` user
document is the default execution document
(`{"sandbox": false, "network": false}`), and no `rust/execution.json` is
written; the flow then stops the daemon. No other test asserts that default
execution document, so a later move has to name the command that keeps that
assertion, not just the startup. The TUI first-run coverage lives in
`cold_hosted_tui_opens_without_reading_the_session_catalog` and
`startup::first_text_or_draft_creates_once_with_startup_options_in_both_tui_modes`.

## Moving a TUI case out of CI

A TUI case may be `#[ignore]`d only when a tmux scenario named in this document
exercises the same user-visible flow, or the behavior has moved to a component
or protocol test. Otherwise it stays selected by CI and #1810 tracks it.

A migrated case may then be deleted when the behavior is no longer a product
promise and the issue that drops it says so. The commit that changes a case's
routing also updates this inventory and the per-gap table. Adding a TUI flow
means adding a tmux scenario: a new `cli_flow` case that drives the UI stays in
CI until one exists.
