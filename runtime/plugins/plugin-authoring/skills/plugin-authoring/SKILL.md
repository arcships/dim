---
name: plugin-authoring
description: Write a TypeScript Dim plugin that adds tools or settings, then install and reload it with the `plugin` tool. Use when asked to write, create, change or install a Dim plugin or a new agent tool.
---

# Writing a Dim plugin

A plugin is a directory. Dim loads its code in its Node runtime; you install it with the
`plugin` tool and reload it after each edit, without restarting Dim.

## Layout

```
echo-plugin/
  dim-plugin.json    the manifest
  index.ts           the entry module (TypeScript, loaded as is; no build step)
  package.json       pins the rutis SDK
  node_modules/      from `npm install`
```

`dim-plugin.json`:

```json
{
  "name": "echo-plugin",
  "description": "Adds the echo tool",
  "code": { "entry": "index.ts", "inject": ["partRegistry", "settings"] }
}
```

- `name`: the plugin's name. Installing a plugin with the name of an existing one replaces it, so
  pick a new name.
- `code.entry`: the entry file, relative to the directory.
- `code.inject`: every service the entry's default export injects, the same list as its `inject`.

`package.json` (the SDK version must be exactly the one Dim runs, 0.17.1):

```json
{ "name": "echo-plugin", "private": true, "type": "module",
  "dependencies": { "@arcships/rutis": "0.17.1" } }
```

Run `npm install` in the directory before installing the plugin.

## The entry module

The default export runs once in the process. It registers a Session part with `partRegistry`;
every Session mounts a copy of the part, and the part registers tools with that Session's
`toolRegistry`.

```ts
import { definePlugin } from '@arcships/rutis'

const settings = {
  source: 'echo-plugin',
  userFile: 'echo-plugin.json',
  schema: {
    type: 'object',
    properties: { prefix: { type: 'string', default: '' } },
    additionalProperties: false,
  },
}

const tools = definePlugin({
  name: 'EchoTools',
  inject: ['toolRegistry', 'settings'],
  async apply(ctx: any) {
    const store = ctx.use('settings')
    await ctx.use('toolRegistry').register(
      {
        name: 'echo',
        description: 'Echo the given text',
        source: 'extension',
        risk: 'session',
        inputSchema: {
          type: 'object',
          properties: { text: { type: 'string' } },
          required: ['text'],
          additionalProperties: false,
        },
      },
      async (call: any) => {
        if (typeof call.arguments.text !== 'string') {
          return { content: 'text must be a string', isError: true }
        }
        const read = await store.read({ source: 'echo-plugin', scope: 'user' })
        const prefix = read.ok ? read.value.document.prefix : ''
        return { content: prefix + call.arguments.text }
      },
    )
  },
})

export default definePlugin({
  name: 'EchoPlugin',
  inject: ['partRegistry', 'settings'],
  async apply(ctx: any) {
    await ctx.use('settings').register(settings)
    await ctx.use('partRegistry').registerDefinition('session', 'EchoTools', tools)
  },
})
```

Rules:

- **Tools.** A tool name is 1–64 letters, digits, `_` or `-`, and must not be taken. `risk` is
  required: `read_filesystem`, `write_filesystem`, `execute_process`, `network` or `session`,
  whichever matches what the tool does; Permission decides each call by it. The handler gets
  `call.arguments` (validate them) and returns `{ content: string, isError?: boolean }`;
  `content` is the only text the model reads.
- **Settings** (optional; for values the user may change). Register each source in the default
  export's `apply` with `settings.register(declaration)`: `source` (lowercase letters, digits,
  `-`, `_`), `userFile` (a file name), `schema` (a JSON Schema whose root is `type: object`;
  give members a `default`), and optionally `userOnly` (JSON Pointers of members only the user
  may change). Give a secret such as an API key `"writeOnly": true` in its schema and list it in
  `userOnly`: `plugin config` then never shows or writes its value. Source names and files are unique on the host. Read the
  document when you need it, as the tool above does, so a changed value applies at once.
  `read` returns `{ ok: true, value: { revision, document } }` or `{ ok: false, error }`.
- **Lifetime.** Do no work at module top level besides imports and constants. Register in
  `apply`; registrations end when the plugin is reloaded, turned off or removed. Write
  diagnostics with `console.warn` / `console.error` (they go to Dim's log).
- **Reload** imports `index.ts` again but not the modules it imports, so keep the code you will
  edit in `index.ts`.

## Install, test, change

Use the `plugin` tool (`help <command>` shows details):

1. `install <path to the directory>`: the result's `status` is `running` when it loaded. On
   `failed`, read `error`, fix the code and run `reload <name>`. `pending` with `waitingFor`
   means a dependency or the Node runtime is not available yet.
2. The new tools are offered from your next model request in this Session; call one to test it.
3. After editing, `reload <name>`.
4. `config <name>` shows the plugin's Settings; `config <name> set <key> <value>` changes one.
5. `list` shows every plugin and its status; `disable <name>` turns one off; `remove <id>`
   uninstalls one.
