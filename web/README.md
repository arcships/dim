# Web workspace frontend

The React 19 and HeroUI frontend opened by `dimcode web`. Vite builds the app, which talks to the App Server over protocol v4 JSON-RPC on `/ws`. See the [Web workspace design](../docs/design/web-workspace.md).

Use the Node version in [`.node-version`](../.node-version) and npm.

## Develop against a running entry

1. Open the web entry from the CLI: `dimcode web --no-open`. It prints the address (default `http://127.0.0.1:43201/`); the link it would open carries `#bootstrap=<token>`.
2. Start the dev server, which forwards `/api` and `/ws` to the entry:

   ```sh
   cd web
   npm ci
   npm run dev                                   # entry on 127.0.0.1:43201
   DIM_WEB_TARGET=http://127.0.0.1:43202 npm run dev   # another port
   ```

3. Open `http://localhost:5173/#bootstrap=<token>` with the token from the CLI's link. The page exchanges it for the access cookie and drops it from the address bar. A page that says the link expired needs a new token (run `dimcode web` again).

The proxy rewrites `Origin` to the entry's own address, because the entry accepts only same-origin requests.

## Build

```sh
cd web
npm ci
npm run build      # React type check and Vite build into web/dist
npm run typecheck  # React type check only
npm test           # Node test runner over unit and component tests, then Vitest over src/**/*.test.ts
```

`web/dist` is the release artifact input. The Web entry serves `index.html`
for the workspace routes and immutable files under `/assets/`. Lucide icons,
fonts and JavaScript ship with the build. Shiki uses its JavaScript regex
engine under the Web entry's CSP. Test the packaged entry by pointing
`DIMCODE_WEB_ASSETS` at the build directory's absolute path when starting
`dimcode --home` with a disposable home.

## Layout

| Path | Purpose |
| --- | --- |
| `src/react/` | Web workspace, settings, virtualized timeline, Lines and composer using React and HeroUI |
| `src/react/workspace.css`, `src/styles/primary-colors.css` | Tailwind CSS v4, HeroUI theme, Dim accent, light and dark surfaces, and responsive layout |
| `src/rpc/client.ts` | WebSocket JSON-RPC client with `hello` v4, timeouts, `daemon.ping` heartbeat, reconnect with backoff and bootstrap exchange |
| `src/rpc/protocol.ts`, `src/rpc/read.ts` | Protocol types and the strict readers that decode every reply and notification |
| `src/projection.ts` | Folds history pages, `session.event` and `session.model.progress` into timeline rows (as `crates/dim-cli/src/projection.rs`) |
| `src/lines.ts` | Folds branch facts into the Session's line tree, line states and cut message counts |
| `src/stores/` | Connection, session directory, attached sessions and their per-line streams, models, layout, toasts and line tabs |
| `src/features/` | Shared view helpers and the previous Vue implementation used by compatibility tests |
| `packages/chat-ui/` | Vue chat timeline package (`@dimcode/chat-ui`) with its own desktop contract and publication |
