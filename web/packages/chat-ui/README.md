# @dimcode/chat-ui

Status: imported from `dim-electron` after the leaf (#524), timeline core (#525), and desktop adapter/parity (#526) work completed. This is the upstream home for the shared chat render layer. The package owns the timeline surface, tool renderer registry/trace, tool card, user-content segments, user-message rail and marks/actions, markdown file chip/reference helpers, and pure row/search/tool helpers. The desktop scroll/restore controller, editor/interaction controllers, and host adapters stay desktop-owned.

This package is the shared, presentational chat render layer for the desktop renderer and the TUI web workspace. It does not own session, run, approval, or model state.

## Extracted leaf surface

- `ToolCodeBlock` — code/diff rendering with host-provided theme and font size.
- `ToolCallInlineSummary` — the tool-card summary line with host labels and icons.
- Tool renderer registry (`createToolCallRegistry`) and trace builder (`buildTimelineToolTrace`) — match precedence, semantic `ChatUiToolLabelKey` labels, primary parameters, detail sections, and params-only input previews for file, command, search, web, todos, ask-user, agent, wait, skill, browser, computer-use, MCP, and generic tools.
- `EditReplacementList` — ordered edit replacements with host code/diff rendering and labels.
- `TimelineToolMediaPanel` — the CUA screenshot/expandable media panel with host labels and image card.
- `TimelineUserBubble` — the user-message bubble shell.
- `TimelineUserContent` — user-message segments (text, skill chips, file/commit mentions, links) with host-supplied skill names, icons, attachment facts, and open-file callback.
- `TimelineUserMarks` — the serialized mark-annotation cards; the host parses the mark blocks, supplies the display fields, and receives the clicked mark.
- `TimelineUserActions` — the user-message time, withdraw, copy, edit, fork, and branch row; the host supplies action flags, copy state, labels, icons, button/tooltip components, and handles the emitted actions.
- `MarkdownHeadingNode` — GitHub-style heading ids.
- `FileChip` — the Markdown file-link/inline-code chip; the host supplies the resolved icon URL, the display label inputs, and the open handler through the `open` payload.
- `TimelineSearchHighlightedText`, `TimelineSearchDomText`, and `TimelineWindowTruncationDivider` — timeline search and bounded-view presentation.
- Pure helpers: file href parsing, file display labels, Markdown file-reference resolution, link-icon classification, Windows Markdown paths, heading slugs, selected-text extraction, timeline timestamps, tool code line stripping, timeline row hashing, search normalization, thinking excerpts, window budgets, turn artifacts, user-message rail markers, tool category/target/work summaries, todo snapshot validation, ask-user answer parsing, and edit replacement parsing.

The desktop renderer keeps thin wrappers where it supplies appearance, theme, or composition context.

## License

The package is licensed under Apache-2.0; see [`LICENSE`](./LICENSE). It was copied from `arcships/dim-electron` (`packages/desktop/packages/chat-ui` at `1a9839f2`), whose repository license is Apache-2.0.

## Boundary

The package may not import:

- desktop `@/` aliases, stores, or `@/i18n`;
- `window.dimcode`, Electron modules, or `getDesktopApi`;
- `@dimcode/shared`, `@dimcode/app-server-protocol`, or `@dimcode/renderer`;
- Pinia or any transport client.

`npm run check:boundary -w @dimcode/chat-ui` enforces the source, test, script, and manifest rules.

## Host contract

`ChatUiHost` is injected through `CHAT_UI_HOST`:

```ts
const host = useChatUiHost()
host.session.value      // normalized ChatUiSessionView | null
host.capabilities.value // interactive and capability flags
host.commands           // send, stop, retry, fork, branch, approve, answer, openFile, copy
host.i18n.t('chatUi.assistant')
```

Each application provides the adapter:

- Desktop adapter: desktop stores and `window.dimcode` map into `ChatUiHost`.
- Web adapter: `useChatStore`, `useSessionsStore`, `useConnectionStore`, and `rpc/client` map into `ChatUiHost`.

The package never opens a socket or reads an application store.

Copy buttons write through `host.commands.copy(text)` when a host is provided, so the host decides how to reach the clipboard (for example outside a secure context). Without a host they fall back to `navigator.clipboard.writeText`. A rejected copy shows the same failure feedback as a clipboard error.

### Non-desktop adapter checklist

`tests/non-desktop-host.test.ts` is the executable recipe for a second host. A web adapter should provide:

- `scope`: display `sessionId`, `branchId`, real `detailSessionId`, and `restoreActive`;
- `actions`: `canEdit`, `canFork`, and a reactive `pending` flag;
- `slots`: `userContent`, `turnOutput`, `markdown`, `media`, and `toolResult`;
- `hooks`: content/media presence, system-notice classification/text, non-rest-point input, agent model label, latest run, and subagent duration;
- `io`: text-attachment read, timeline message detail, and run changed paths;
- `effects`: open-plan, success/error notification, file-changes visibility, and perf hooks.
- top-level `session`, `capabilities`, `commands`, and `i18n` from the web stores/client; read-only mode uses `interactive: false` with callable no-op commands.

The fixture deliberately supplies every optional field, so a contract addition fails the package test until the adapter recipe is updated.

## i18n

The package owns the `chatUi.*` key set and its English reference messages. The host supplies the active locale through `ChatUiI18n`; do not import the desktop application catalog. The desktop `chatUi.timeline.*` mapping and parity test live in #526; a non-desktop host maps the same keys from its own catalog.

## Build

Run from `web/`:

```sh
npm run build -w @dimcode/chat-ui
npm run typecheck -w @dimcode/chat-ui
npm test -w @dimcode/chat-ui
npm run check:boundary -w @dimcode/chat-ui
npm run check:budget -w @dimcode/chat-ui
```

Workspace consumers resolve the package source (`src/index.ts`), which imports the package CSS. The release build emits ESM, declarations, and a separate `dist/chat-ui.css`; `arcships/dim#1839` switches the published entry points to that output and narrows the pack to `dist/`. When that switch happens, every host must import `@dimcode/chat-ui/styles.css` explicitly, because the extracted JS does not import its CSS. The default bundle budget is 900KB gzip; override it with `CHAT_UI_MAX_GZIP_BYTES`.

The web integration must keep the `@terrastruct/d2` alias in its Vite config. Without it, the shared markdown surface pulls an extra D2 chunk (about 8.2MB minified / 6.0MB gzip in the probe).

## Release

The package is private until `arcships/dim#1839` publishes it from the `dim` release train. Until the release and the desktop removal land, `dim-electron` remains the initial source mirror at `1a9839f2`; do not edit both copies independently. The follow-up removes the desktop copy and pins an exact published version.

## Tracking

- Upstream parent: `arcships/dim#1836`
- Upstream package home/release: `arcships/dim#1839`
- Upstream read-only integration: `arcships/dim#1837`
- Upstream interactive commands: `arcships/dim#1838`
- Desktop parent: `arcships/dim-electron#522`
- Desktop skeleton/leaf/core/adapter: `#523`/`#524`/`#525`/`#526` (complete)
- Temporary source mirror until publication: `arcships/dim-electron/packages/desktop/packages/chat-ui`
