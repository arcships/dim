# Timeline-first TUI: branches and forks derived from the Timeline

Status: Proposed. Builds on [Timeline branches](timeline-branches.md) ([#427](https://github.com/arcships/dim/issues/427)) and [Session fork](appserver/session-deletion-and-fork.md). Scope: the CLI TUI's information hierarchy and interaction, and the client-side data it folds. No runtime, event, storage or App Server method changes. Chinese: [中文](../zh/design/timeline-ui.md).

## 1. Problem

After #427 the TUI had three unrelated entries: the conversation (`main`'s transcript), a `/lines` panel next to `/tasks`, and no TUI entry for `session.fork` at all. A user could not see where a line grew from, and could not open a line from a point in history (#427 S3).

In the data these are one thing: **a branch and a fork both cut a Timeline at a sequence.** `branch/create` carries `at`, and both `session.branch_create {parentBranchId, at, prompt}` and `session.fork {sessionId, at}` take `at`. The Timeline is therefore the primary axis; branching and forking are actions on a point of it; the lines panel is the map of what those actions produced.

## 2. Hierarchy

```
L0  Session ............ the subject: identity, workspace, interaction surface
 └ L1  Timeline ........ the shown line's transcript; the only main surface
     ├ point ........... a cut: the turn boundary just before a user message
     │   ├ e edit / q quote        (existing)
     │   ├ b branch here → a line in this Session
     │   └ f fork here   → a new Session (main only)
     ├ marker .......... ⑂ a line opened here (pickable)
     └ /timeline ....... every point of the shown line, to jump to (§4.6)
 L3  Lines (/lines) .... every line under the line it was cut from
```

1. **One main surface.** The screen always shows one line's Timeline. Changing lines changes which Timeline is shown, not the screen.
2. **Derive from a point.** Branching and forking start from a point of the shown Timeline. Its end is a point too (`/branch`, `/fork`).
3. **Results return to a Timeline.** A branch leaves a `⑂` marker on its parent and answers in a side panel while the screen stays on the parent (§4.7); a fork attaches to the new Session's Timeline.

## 3. Branch versus fork

Both share the point → derive entry. They differ in where the result lives (#427 §2):

| | Branch (`b`) | Fork (`f`) |
| --- | --- | --- |
| Result | A line of this Session | A new Session |
| Shares | The workspace and the subject; talks with its parent | History up to the cut only |
| Points | Any open line, at a valid context boundary | `main` only, at a rest point |
| After | The screen stays; the new line answers in the side panel (§4.7) | The TUI attaches to the fork |
| Trace on the source | A `⑂` marker | None (§7) |

An action that does not apply says why in a notice (`A fork copies main; b opens a line here`, `Line has ended: X`). The App Server stays the only judge of a valid cut; its refusal is shown as a notice.

## 4. Timeline and points

### 4.1 Point

The fullscreen pick (Tab) already stops on user messages. A picked message's point is the sequence just before it: "before this message was said". It ends an atomic batch, usually passes `validate_context_boundary`, and on `main` is usually a rest point; branching there with a reworded message is S3's retry-and-compare. A message queued while a turn ran cuts before its submission, not where it entered the transcript; that cut lies inside the running turn, so a fork there is refused, and a line only where the context boundary allows.

The end of the shown line is a point as well: its newest turn end, used by `/branch` and `/fork`. With no finished turn there is no point (`No finished turn to cut at yet`).

```
┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓
┃ › try RwLock for the cache lock                           ┃
┗━e edit · b branch · f fork━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛
```

A box too narrow for the hint shows `e edit`.

### 4.2 `b`: branch here

`session.branch_create` needs the line's opening input, so `b` puts the picked message in the composer and the composer's next submit opens the line:

```
╭──────────────────────────────────────────── provider / model ─╮
│ › try RwLock for the cache lock█                              │
╰───────────────────────────── ⑂ new line from main · Esc cancel ─╯
```

`Enter` sends `session.branch_create {parentBranchId: shown line, at, prompt}` and the new line answers in the side panel (§4.7). `Esc` cancels. A session or line switch drops the draft.

### 4.3 `f`: fork here

On `main` only, from a Tab pick or `/timeline` (§4.6), which is how scrollback picks a message. A bordered confirmation names the cut and explains that the source Session stays unchanged. Cancel is selected initially; Up/Down or Tab selects an action, Enter confirms, `1` or `y` creates the fork, and `2`, `n` or Esc cancels. Other keys do not dismiss it. The question and actions are in the body, not the composer's border:

```text
╭─ Fork session ─────────────────────────────────────────────╮
│ Fork the session before “try RwLock for the cache”?         │
│ Creates a new session; the current session stays unchanged.│
│   1. Create fork                                           │
│ › 2. Cancel                                               │
│ ↑↓ select · Enter confirm · Esc cancel                     │
╰───────────────────────────────────────────────────────────╯
```

For `/fork`, the question is `Fork the session after the last turn?`. Confirmation sends `session.fork {sessionId, at}` and attaches to the fork under a `forked ID · N messages` boundary, with the picked message in its composer. A fork starts no work until the user submits (fork design §6.5). Cancelling returns to the Timeline action list when opened there, otherwise to the original input. The draft and transcript position are preserved.

In scrollback the dialog replaces the composer in the live interaction region, never entering committed history. In fullscreen it is an overlay. Both modes use the same choices and keyboard behavior; narrow dialogs wrap the question and retain selectable actions.

### 4.4 Markers

`branch/create` is written on the parent at the moment the line opens, which for a past cut is the parent's tail, not the cut. Committed scrollback is append-only, so the marker stays where the fact is and names its cut instead:

```
 › find the leak
 … reply …
 › try the parser
 … reply …
 ⑂ try the parser via cache · before “try the parser” · f7f0a984
```

A line is named by the first line of its purpose; its short id follows, and names a line opened without one. `before “…”` appears when the cut is just before a message of this transcript; a line opened at the tail (a `branch` tool call) has none. The marker is a pick stop like a member card: `Enter`, or a click, shows the line. Ends are not marked: the lines panel shows them, and `main` already receives notices for its tool lines.

### 4.5 Where am I

While a line is shown, the composer's bottom border names its path from `main`, each line by its name (§4.4), and its cut:

```
╰── ~/repo (main) · ⑂ main ▸ parser race ▸ the guard · after message 4 ─╯
```

`after message N` counts the user messages in the parent's history, its inherited ones included, at the cut.

### 4.6 `/timeline`: jumping to a message

The Timeline's points are its loaded user messages. Tab walks them one at a time from the newest; `/timeline` lists them as the desktop client's message rail does: each row is the message and the first text of its answer. When older history remains on the server, the first item is **Load earlier history**; Enter requests one earlier page without closing the panel. Message numbers are relative to the loaded history.

```
┌ Timeline ────────────────────────────────────────────────────────┐
│   #1  weather in Shanghai      Cloudy, 20°C, light wind …        │
│   #2  try RwLock for the cache The lock moves into …             │
│ › #3  adsa                                                       │
└────────────────────────────────── Enter actions · Esc close ┘
```

- The newest message is selected. Typing does not filter: the letters act on the selected message as they do on a pick (§4.1–4.3).
- `Enter` opens a bordered action list naming the selected message: Jump to message, Edit as a new draft, Quote message, Create branch from here, Fork session. The fork choice states that its cut is before the selected message. Up/Down and Enter select an action; numbers 1–5 select directly. Existing `e`, `q`, `b` and `f` shortcuts work in both lists. Esc returns to the message list with its selection and list position intact, then closes it. Fullscreen shows the lists as overlays; scrollback shows them in the live interaction region without committing them to history.
- Jump closes the panel. Fullscreen picks the message, as Tab would, and the pick keys apply. Scrollback cannot move up: a message it has not drawn yet is drawn from there on as an earlier-history page; one already drawn gets a notice that `/tui` can jump to it.

A resume requests the newest page of the latest turn through the [history window](appserver/session-history-window.md#6-client-flow), pinned to the attach reply's sequence. It does not scan the whole transcript before displaying it. Further pages use `before` and the same `through`: `latestTurn` remains set until that window is complete, then is omitted to reach older turns. Live events are merged once above the pin. A daemon without `session.history.latestTurn` keeps the existing forward read.

Fullscreen tops the transcript with a load-more row (`↑ Earlier history · Enter or click to load`) when server history remains; it does not invent a message count for unread pages. A click, or Tab up to it and Enter, requests one earlier page and keeps the previous top message in view. Scrollback loads through `/timeline`, then uses its existing jump action to draw earlier messages. Locally retained history still draws in groups of 100 messages. Opening `/tasks` or `/lines` requests the remaining history for the inventory, including branches discovered in those pages; simply opening the conversation does not.

`/goal` reads the branch's complete goal history on demand before deciding
whether the requested change edits an active goal or sets a new one. An active
goal may predate the initial turn window.

```text
attach -> history {latestTurn:true, through:H} -> newest page
               live > H ---------------------> merge once
load earlier -> history {before:F, through:H} -> prepend, retain view anchor
```

The client reads only RPC replies. Timeline and TimelineTree ownership and persistence reads remain inside the App Server; no TUI history path opens the database.

### 4.7 `/branch <prompt>` and the side panel

`/branch` takes its prompt inline, the way a side question does. The palette lists the argument, and `Tab` or `Enter` on a command that takes one fills the composer instead of running it, leaving the argument as a hint:

```
╭──────────────────────────────────────────── provider / model ─╮
│ › /branch <prompt>                                            │
╰───────────────────────────────────────────────────────────────╯
```

`Enter` on `/branch try the parser via cache` sends `session.branch_create` at the end of the shown line (§4.1) in one step; there is no second draft. A run on the shown line keeps going: the command is not queued and does not interrupt. `/branch` with no prompt stays in the composer and says `/branch needs <prompt>`.

The new line answers in a panel while the screen stays on the parent. In fullscreen the panel sits above the composer:

```
 … the parent's transcript keeps streaming …

╭─ ⑂ b-7c · try the parser via cache ─────────────────────────╮
│ ⑂ working…                                                   │
╰──────────────────────────────────────────────── Esc close ─╯
╭──────────────────────────────────────────── provider / model ─╮
│ ›                                                             │
╰───────────────────────────────────────────────────────────────╯
```

```
╭─ ⑂ b-7c · try the parser via cache ─────────────────────────╮
│ ✓ Explored 2 files ▸                                         │
│                                                              │
│ The lock is held across the await in cache.rs:88; moving the │
│ parse out of the guard removes the race. Patched and tested. │
│ ⑂ done                                                       │
╰──────────────────────────────────────────────── Esc close ─╯
```

1. The panel draws the line's transcript as the screen draws one, without the opening input its title names: thinking, tool calls at the tool detail setting, and replies as they arrive. It shows the tail when longer than a third of the screen, and the line's state on its last row: `working…` while a turn runs, `done` once it ended and the line waits for input, or how it ended.
2. `Esc` closes the panel before it does anything else. The line keeps running; closing it only stops the panel following it. The `⑂` marker (§4.4) is what stays in scrollback, and `Enter` on it, or `/lines`, shows the line.
3. One panel at a time: another `/branch` or `b` replaces it. Showing a line, switching Session or `/new` closes it.
4. In fullscreen the panel pushes the transcript up, and closing it brings the transcript back down onto the composer. Scrollback never lifts the composer, so there the panel opens below it, where menus open, and closing it leaves no gap above the composer.
5. `/branch` that cannot open a line (no finished turn yet, the shown line ended) keeps its draft in the composer and says why.

### 4.8 A message the model has not read yet

A user message is written, applied and then consumed. The client draws the row from `input/applied`, and the row shows whether the model has read it yet:

| State | Fact | The model has read it | Drawn as |
| --- | --- | --- | --- |
| queued | `input/message`, InputQueue holds it | no | the queue strip above the composer, while the line runs |
| applied | `input/applied`, waiting for Loop's next request | no | a pending message row |
| consumed | `input/consumed`, in the request that carries it | yes | a user message row |

A pending row keeps its text and its cut, and draws in the muted colour without the band. In fullscreen the row is committed where the message was said, and `input/consumed` redraws that block in place; leaving fullscreen settles a committed pending row first, because scrollback cannot take the hint back. Scrollback is append-only, so there a row that can still change stays in the live tail (as a streaming answer and a running tool group do) and enters scrollback once it settles, in the user colour. The tail is drawn after everything committed, so a pending row stays there only while it is the newest: when a later row settles first — an applied input that outlives a stop — the message is committed in Timeline order in the user colour, and scrollback cannot take the hint back. A message waiting for the next request therefore reads as unanswered wherever the drawing can still change it:

```
 › how do we fix this deadlock                       the model has read this
 › add the missing test too                          still muted: not read yet
```

An applied input can stay pending across a stop, which is the state the next turn consumes ([interrupt](../architecture/rust-agent/interrupt.md)).

A row the interface commits itself, outside the transcript rows — a notice, a `!!` run, the failure line, the boundary of a resume — is not part of that ordering, and can stand above a pending message.

The pending style is the terminal UI's: Web's v1 `session.event` filters `input/consumed` ([event-driven flows](../architecture/rust-agent/event-driven-flows.md)), so a Web message still reads as sent.

The row is keyed by input id from the `input/message` that wrote it to the `input/consumed` that settles it, so a reattached client that folds the same events draws the state it reached, not the state at the time of the write.

## 5. Switching lines

Unchanged from #427 §14: a switch writes a `⑂ name · id` boundary and the shown line's transcript, laid out once that line's history has loaded, which holds its own events only (each branch numbers its own sequences, [sessions service D11](appserver/sessions-service.md)). Its inherited history is its parent's, one marker away.

## 6. Lines panel

`/lines` is the map of the Timelines. Children are ordered by where they were cut from their parent, then by opening order. Each row leads with the line's purpose and says `after message N` and its id:

```
┌ Lines ───────────────────────────────────────────────────────────┐
│   main                  the session itself                       │
│     docs pass           after message 1 · b-02 · ended itself    │
│     parser race         after message 3 · b-7c · working · shown │
│       the guard         after message 4 · b-c3 · working         │
└──────────────────── Enter show the line · x end it · Esc close ──┘
```

Forks are other Sessions and belong to `/resume`. Keys stay as #427 §14 defines them.

## 7. Client data

| # | Need | Owner | Notes |
| --- | --- | --- | --- |
| R1 | `LineView.cut`: the parent sequence and the message count at it | CLI projection | Folded from `branch/create.at` and the parent's user inputs; no format change |
| R2 | A user message row's cut (`ViewRow.cut`), whether the model has consumed it (`ViewRow.pending`), and a marker row's line (`ViewRow.line`) | CLI projection | From sequences and input state the client already has |
| R3 | `session.branch_create` and `session.fork` calls | CLI session host | Existing methods; a request made from a stale attachment is refused locally |
| R4 | Later, optional: show a fork's origin (`↗ forked` on the source, `forked from S` in the fork) | App Server, needs a format decision | `session/forked` carries no data and the fork design adds no provenance column |

## 8. Not in scope

- **Rewinding `main`.** #427 §3: not a branch. "Start over from here" is `b` (compare in this Session) or `f` (a new Session).
- **Compare and adopt.** S3's second half needs its own design. Sibling lines cut at one point are what a comparison will show.
- **A drawn DAG.** Scrollback is append-only; topology is markers, the panel and the path.

## 9. Ablation review

- No new mode: points reuse the Tab pick, line markers and the load-more row reuse the card pick, the switch reuses #427's, the fork's attach reuses `/resume`'s. The one new panel, `/timeline`, lists the points the pick walks and takes the pick's keys.
- New interaction: `b`/`f` on a pick, the composer's line draft, the fork confirmation, the Timeline action list, the path border, `/branch` and `/fork`. The confirmation makes the cut and consequences visible; the action list makes the actions discoverable without reading shortcut hints. Both reuse existing prompt rendering and leave no durable history.
- The side panel (§4.7) is the one new surface: following a line without leaving the parent needs a view of the line that the parent's Timeline cannot hold. It folds nothing new; the CLI already keeps a transcript per opened line. Its collapsed form is the existing `⑂` marker, so dismissal writes nothing.
- Considered and dropped: a `b` key in the lines panel (needs each line's head; `/branch` on the shown line covers it), greyed hints per point (a notice on the key says the same with less state), end markers (§4.4), a one-row summary of the inherited prefix and `Esc Esc` back to the parent (§5 and the marker already lead there).
- The pending message (§4.8) adds no list of its own: it is the same row, keyed by input id, and the queue strip is that row's earlier state. Scrollback's live tail and fullscreen's in-place redraw are the rules a streaming answer, a running tool group and a delegation card already follow, and the three states come from events the client already receives — no timer, no new event, no second transcript.

## 10. Decisions

1. A point is the boundary before a user message, plus the line's end. A boundary after an answer is not offered.
2. `b` and `f` carry the picked message into the composer, as `e` does.
3. R4 waits for a format decision.
4. Opening a line keeps the screen on its parent; the line answers in the side panel (§4.7).
