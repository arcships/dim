# Web workspace: TUI parity plan

Status: Proposed. Related: [Web workspace](web-workspace.md) (the design this plan extends), [Questions](plugins/Questions.md), [CLI commands](cli-commands.md).

The TUI registers 36 commands and several interaction flows that the web workspace does not reach. This plan lists the gaps, ranks them, and fixes what the first change implements. Each item names the App Server methods it uses; none needs a new server method unless the table says so.

## 1. Priorities

| Priority | Item | TUI entry | Web today | Methods | Status |
| --- | --- | --- | --- | --- | --- |
| P0 | Answer `ask_user` questions | Question prompt | No UI; a model question waits until the turn is stopped | `session.question.respond` | Implemented in this change |
| P0 | Approval: always allow a simple command | `[y/a/N]` | Only "Allow once" and "Reject" | `permission.rules`, `permission.set_rules`, `approval.respond` | Implemented in this change |
| P0 | Approval: allow a file path for this Session | Permission prompt, grant option | Not offered | `session.grants.add`, `approval.respond` | Implemented in this change |
| P1 | Compact the context, show usage, fork the Session | `/compact`, `/usage`, `/fork` | Not offered | `session.context.compact`, `session.context.usage`, `session.fork` | Implemented: command menu, main Session composer |
| P1 | Rename the Session | `/rename` | Not offered | `session.rename` | Blocked: the App Server does not serve `session.rename` yet (SessionTitle design) |
| P1 | Session goal: set, edit, pause, resume, clear | `/goal` | Not offered | `session.goal.set`, `session.goal.edit`, `session.goal.pause`, `session.goal.resume`, `session.goal.clear` | Implemented: the goal is folded from the line's events (`goal.ts`), command menu |
| P2 | Attachments: pasted images and long pastes, dropped images, `@` file mentions | Composer | Text only | `session.submit` (`user.content` with image blocks), `attachment.put`, `session.files.search` | Implemented: images pasted or dropped, pastes over 16 KiB stored as attachments when the Session serves `attachment.put`, `@` mentions from `session.files.search` (a new Session method on WorkspaceTools, the TUI's index and ranking) |
| P2 | Reasoning effort in chat | `/effort` | Only in provider preferences | `session.model.update` (`modelPreferences`), `provider.catalog` | Implemented: command menu, main Session composer |
| P2 | Steer the run and withdraw queued input | `/steer`, Ctrl+S | Queued messages not shown; only Stop | `session.input.withdraw` | Implemented: queued strip above the main composer (Steer for the oldest, Remove for any) |
| — | Fast: service tier | `/fast` | Not offered | — | Dropped: no model advertises a service tier yet |
| P3 | Background task output and stop | `/tasks` | Not offered | `session.exec.output`, `session.exec.stop` | Planned |
| P3 | Workflow status and stop | Workflow view | Not offered | `workflow.status`, `workflow.stop` | Planned |
| P3 | Session agent switch | `/agent` | Default agent in Settings only | `agent.list`, `session.agent.update` | Planned |
| P3 | Session permission controls | `/permissions` | Permission preset picker, with process isolation linked to its existing settings; see [Web workspace §4](web-workspace.md#4-frontend) | `session.permission.get`, `session.permission.update`, `permission/changed` | Implemented |
| P3 | Hooks settings | `/hooks` | No page | `hooks.catalog`, `hooks.save`, `hooks.set_enabled` | Needs a Settings allowlist change |
| P3 | Jump to a message | `/timeline` | Implemented as dim-agent's message rail on the timeline (`/timeline` is not offered: dim-agent has no such command) | None | Implemented |
| P3 | Session MCP state | `/mcp` | Global MCP settings only | `session.mcp`, `session.mcp.state` | Planned |

Ranking rule: a gap that stops a run (P0) comes first. Gaps that need only one existing method and no new UI decision come next (P1). Gaps that need protocol checks or new composer controls follow (P2). Settings pages and secondary views come last (P3).

## 2. P0 design

### 2.1 Questions

- A question is pending from `step/tool-call` until the batch's `step/tool-result` (see [Questions §3.2](plugins/Questions.md#32-clients-attach-and-several-clients)). The web workspace decodes `step/tool-call` for the first time; before this change it treated that event as unknown.
- The web client parses `ask_user` input at the wire boundary, with the limits of [Questions A.1](plugins/Questions.md#a1-tool-ask_user): 1 to 4 questions, 2 to 6 options, unique labels, a header of at most 40 characters. A call that fails these limits gets no prompt; the server rejects it at once.
- The prompts replace the composer, as the approval prompt does, on the main line and on every line panel. Every pending call on the shown branch has its own independently answerable card in a scrollable area, so an answered call waiting for batch settlement never hides another call in the same batch. Each answers with `session.question.respond` using `branchId`, `requestId` and `callId`. Decline sends `declined: true`.
- A single-choice question accepts either one option or custom text: choosing an option clears custom text, and entering nonblank custom text clears the selected option. Multiple-choice questions may combine options and custom text.
- `question_not_pending` keeps the prompt open. The prompt closes when the batch result arrives, on every client.

### 2.2 Approval options

- "Always allow" appears when the server supports `permission.rules` and `permission.set_rules`, and the call is `exec` or `execute` with one simple command (no `; & | \` ( ) < > $` or line breaks). It writes an exact allow rule for the command's words unless an identical rule exists, then answers `allow`. A revision conflict or failed write leaves the approval open and shows the error.
- "Allow for this session" appears for `read`, `write` and `edit` calls with a `path`. It calls `session.grants.add` with kind `file` and mode `read` or `write`, then answers `allow`. If the grant is not saved, the approval stays open and the error shows.

These two rules match the TUI's behavior in `crates/dim-cli/src/permission.rs` and `session_host.rs`, except that the TUI still allows once when a rule write fails. The web prompt does not, so a failed write never turns into an unplanned allow.

The web prompt saves only the exact rule. The TUI also accepts a prefix rule typed by the user; that stays open.

## 3. Decisions still open

All P2 items are implemented or dropped (`/fast`). The P3 items remain planned; see §1.

## 4. Protocol checks

- Attachments: `session.submit` accepts image content blocks in `user.content`, as the TUI's `submit_draft` sends them. The server's image size limit is not checked: the TUI scales images before sending, the web sends them as picked.



## 5. Verification

- `npm run typecheck` and `node --test` over the existing web tests.
- A scratch replay of recorded `session.event` sequences through the projection, covering a question that is answered, declined, and settled by another client.
- A browser check against a running `dimcode web` with an offline model that calls `ask_user` and an approval for `exec` and `read`.

## 6. Out of scope

- Terminal-only commands: `/tui`, `/keys`, `/exit`, `/scroll-top`, `/scroll-bottom`, `/web`. The web workspace has its own shell and browser scrolling.
- Commands the web workspace already covers through Settings: `/connect`, `/login`, `/models`, `/mcp`, `/plugins`, `/skills`, `/reload-skills`, `/theme`, `/language`, `/settings`.

## 7. Remaining

- P3, not started: background task output and stop (`/tasks`), workflow status and stop, session agent switch (`/agent`), session MCP state (`/mcp`), and Hooks settings (needs a Settings allowlist change in `web-workspace.md` §4.3 first).
- `/rename` waits for the App Server's `session.rename` (SessionTitle). The sidebar and header still show the preview, not a manual title.
- Not yet run end to end: none of the P0 to P2 features has been exercised in a browser against a running App Server. `session.files.search` is compiled and unit-checked, not called over RPC.
- Known gaps inside the done items: images are sent without the TUI's scaling; `@` links are matched by their text, so a user who edits a mention's text loses the link; the queue strip covers the main line only, and Steer applies to the oldest message only.
- The approval prompt's member panel still has English-only strings from before this plan ("Retry", "Subagent tool approval", "Loading member action...").
