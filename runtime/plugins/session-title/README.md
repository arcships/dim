# SessionTitle

Optional Session TypeScript package implementing the [SessionTitle contract](../../../docs/design/plugins/SessionTitle.md).
The original C4 row provides `sessionEvents:session-title` and `sessionMethods:session.rename`;
the existing process services own history, settings, model execution and title persistence.

## Loading

Install the shared runtime and this package's pinned SDK dependencies:

```sh
npm ci --prefix runtime --ignore-scripts
npm ci --prefix runtime/plugins/session-title --ignore-scripts
(cd runtime && npm install --no-save --package-lock=false --ignore-scripts --install-links ./plugins/session-title)
```

Pass the complete [session-title.json](session-title.json) row to
`ServerConfig.session_typescript.rows`, with `anchor` pointing to the embedding host's
`package.json` (`runtime/package.json` here). The package name resolves through its export.
The module and host use the same `inject`/`provides` declaration and Settings source.
This is an explicit optional installation, separate from the default CLI profile and release payload.
The `dim-app-server` binary takes such rows with `--session-rows FILE` (a JSON array whose
file location is the anchor); see the [App Server README](../../../crates/dim-app-server/README.md#session-typescript-embedding).

Required named dependencies are `sessions`, `settings`, `sessionMetadata` and `auxiliaryModel`.
An unavailable provider keeps this optional row Pending, including rename. Native Sessions
continue under existing assembly rules. Events missed while the row is unavailable are not replayed;
the next adopted operator input can trigger an attempt using the first adopted input in history.

## Behavior

An interactive leader's own `main` adoption schedules owned asynchronous work and returns
from the broadcast callback. A fixed published history prefix supplies only the first adopted
operator input's text, bounded to 2000 Unicode code points. Images, raw blocks, deleted inputs,
later input text and tool/assistant/system content are excluded. An image-only first input
produces no automatic title and is remembered for this load. External-agent Sessions skip
repeated history reads until a later adopted model notice permits another decision.
An explicit external-agent title model is also skipped; this exclusion is not cached across
Settings changes, so selecting a native title model permits the next attempt.

Each attempt first reads the validated user-only `session-title` Settings document; disabled
generation or an explicit external-agent title model reads neither metadata nor history. Defaults are
`enabled: true`, `maxLength: 80` (10–200 code points) and `model: null`. The native backend
stores it in `user/session-title`; the file backend uses the user file `session-title.json`.
Use existing `settings.read`/`settings.update` with the returned revision. Project scope is
rejected. Business edits affect the next attempt or rename without reloading the row.

With `model: null`, the latest adopted `context/configured` in the fixed prefix supplies the
model. A prefix without one fails the attempt; creation configuration and Provider defaults
are not substitutes. The request omits `maxOutputTokens` and uses S4's existing model limit.
Generation has a 30-second budget and three failed attempts per loaded generation. The package
emits outcome/duration diagnostics to stderr. The supplied C4 host currently discards runtime
stderr; it does not expose these diagnostics to clients.

`session.rename({sessionId, title})` trims and validates a manual title; `null` clears it.
Manual conflicts allow three retries. An automatic result uses the revision read before
generation, so a concurrent manual write wins. Existing SessionStore notices and list fields
announce committed titles. Titles add no Timeline facts or main-model context.

Closing cancels and joins the original auxiliary operation through S4's existing reference.
After `setTitle` handover, SessionStore settles the mutation; a lost reply permits no repeat
or rollback. Generation takes no Session activity hold. Clearing permits a new attempt on
the next adopted operator input, still using the original first input.

## Verification

```sh
node runtime/node_modules/typescript/bin/tsc -p runtime/plugins/session-title/tsconfig.json
node --test runtime/plugins/session-title/session-title.test.mjs
```

The SDK flows execute the actual plugin over its declared ports. The real Node/App Server flow is
`node_session_title_generation_rename_settings_restore_fork_and_close` in
[session_title.rs](../../../crates/dim-app-server/tests/transport_flow/session_title.rs), run with
the same `DIM_TEST_NODE`, `DIM_TEST_JS_RUNTIME` and `DIM_TEST_RUTIS_SDK` settings as public Node CI.
It uses disposable stores and existing Prompt adoption, Provider, Settings, S2 and metadata bindings.
