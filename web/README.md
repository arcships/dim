# Web workspace frontend

The browser frontend that `dimcode web` opens: a Vue 3 + Vite app that speaks App Server protocol v3 (JSON-RPC over the web entry's `/ws`). The design is [docs/design/web-workspace.md](../docs/design/web-workspace.md) §4.

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
npm run build      # vue-tsc type check, then vite build into web/dist
npm run typecheck  # type check only
```

`web/dist` is what the entry serves: `index.html` for every non-asset path and `/assets/*` as immutable files. The build keeps to the entry's CSP: no inline scripts, no `data:` fonts, and Shiki highlights with its JavaScript regex engine (WebAssembly would need `wasm-unsafe-eval`).

## Layout

| Path | Purpose |
| --- | --- |
| `src/rpc/client.ts` | WebSocket JSON-RPC client: `hello` v3, timeouts, `daemon.ping` heartbeat, reconnect with backoff, bootstrap exchange |
| `src/rpc/protocol.ts`, `src/rpc/read.ts` | Protocol types and the strict readers that decode every reply and notification |
| `src/projection.ts` | Folds history pages, `session.event` and `session.model.progress` into timeline rows (as `crates/dim-cli/src/projection.rs`) |
| `src/stores/` | Connection, session directory, attached sessions, models, layout |
| `src/features/chat/` | Chat page, new-session page, timeline, composer, model menu, approval prompt |
