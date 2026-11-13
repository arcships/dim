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
npm test           # Node test runner over unit and component tests
```

`web/dist` is what the entry serves: `index.html` for every non-asset path and `/assets/*` as immutable files. The build keeps to the entry's CSP: no inline scripts, no `data:` fonts, `@nuxt/ui` v4 icons are bundled offline from `@iconify-json/lucide` (network Iconify API providers are disabled in `main.ts`), and Shiki highlights with its JavaScript regex engine (WebAssembly would need `wasm-unsafe-eval`). `vite.config.ts` replaces markstream's optional `@terrastruct/d2` renderer with an inert module; that renderer is intentionally not shipped, so removing the alias pulls it into the bundle.

## Layout

| Path | Purpose |
| --- | --- |
| `src/styles/global.css` | Tailwind CSS v4, `@nuxt/ui` v4 theme tokens (`--ui-*`, `--text-*` scaled by `--ui-font-size`), light/dark palettes and layout rules |
| `src/rpc/client.ts` | WebSocket JSON-RPC client: `hello` v3, timeouts, `daemon.ping` heartbeat, reconnect with backoff, bootstrap exchange |
| `src/rpc/protocol.ts`, `src/rpc/read.ts` | Protocol types and the strict readers that decode every reply and notification |
| `src/projection.ts` | Folds history pages, `session.event` and `session.model.progress` into timeline rows (as `crates/dim-cli/src/projection.rs`) |
| `src/lines.ts` | Folds branch facts into the Session's line tree, line states and cut message counts |
| `src/stores/` | Connection, session directory, attached sessions and their per-line streams, models, layout, toasts and line tabs |
| `src/features/chat/` | Chat page, new-session page, timeline, composer, model menu, approval prompt |
| `src/features/lines/` | Right-side Lines panel, tree, line conversation, new-line and end-line dialogs |
| `src/features/settings/` | Account, appearance, agent defaults, providers, modality, MCP, plugins, skills, permissions, scheduled tasks, archived sessions and diagnostics |

The conversation work-status presentation is visually inspired by [Rare UI AI Status](https://www.rareui.com/components/aistatus), implemented with Vue transitions and local icons. Active groups show the current action; completed groups retain their expandable activity summary. Reduced-motion preferences disable the animations.
