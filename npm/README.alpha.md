# Dim

Dim is a Rust coding agent with a terminal scrollback interface.

Install the Rust alpha with Node.js `^22.19.0 || >=23.11.0` and optional dependencies enabled:

```sh
npm install -g dimcode-alpha@alpha
dima
```

The alpha installs only `dima` and uses `~/.dimcode-alpha`. It can coexist with the earlier CLI; sessions and account files are not automatically migrated.

The `dimcode` package and `dim` command remain available:

```sh
npm install -g dimcode
dim
```

The Rust CLI has not been published to `dimcode` yet. That package currently distributes the earlier TypeScript CLI. Use `dimcode-alpha@alpha` for this Rust implementation.

The platform package contains the native CLI and App Server; installation does not download executables or run install scripts.

Use `dima --help` for configuration and session commands. Platform packages are built for macOS arm64/x64, Linux arm64/x64 and Windows arm64/x64. Availability depends on verified release artifacts.

TypeScript plugins can import named-service types with
`import type { PluginFiles, Settings, Sessions, AuxiliaryModel, ToolRegistry, SessionMethods, GuideEntries } from 'dimcode-alpha/plugin'`.
See the [plugin author's guide](https://github.com/arcships/dim/blob/main/docs/typescript-plugin-authoring.md).
The type-only entry adds no runtime code or plugin registration.

Dim's original source uses MIT. See `NOTICE` for distribution attribution. Native platform packages include `licenses/RUST-THIRD-PARTY-NOTICES.txt`, terminal library licenses under `licenses/grok-build/`, and Aimux's own MIT license under `licenses/aimux/`. The optional `dimcode-web` package includes `web/THIRD-PARTY-NOTICES.txt`, covering its JavaScript dependencies and Geist fonts under their own licenses. Bundled plugins retain the license files supplied by their npm packages.

### Share an App Server

On the computer you want to control, open `/settings → Remote access` and turn on **Allow connections to this computer**, or run `dima share enable` in a shell. Both show the reachable addresses and the complete token. The token is readable on a narrow screen by selecting it in Settings. Sharing starts off and stays available when you exit the TUI while the App Server is running.

On the computer at the keyboard, start `dima` and open `/devices` from Home or a conversation. Select a computer or choose **Add by address**. On first use, enter the token displayed on the other computer. Leave **Remember on this computer** selected to reconnect using the saved credential.

```text
/devices
  +-- This computer
  +-- Office PC
        +-- Active session   /srv/project  -> open conversation
        +-- New session...                 -> browse from root
                                              or enter an absolute path
                                              -> Start a session here
```

The lower composer border shows the selected computer beside the project path, for example `Office PC:/srv/project`. A new workspace stays empty until the first message or shell command. Open `/devices` again to switch computers. An interrupted connection offers to reopen the same session with its unsent draft.

For a remembered computer, `dima --target "Office PC"` opens its active sessions directly. Add `--resume SESSION_ID` or `--cwd /srv/project` to open a specific conversation or directory. Use the same target option for scripts, for example `dima --target "Office PC" --cwd /srv/project exec -p "Run the tests"`.

On the computer at the keyboard, run `dima share discover` to see LAN candidates, or use an address from the other computer directly. A Tailscale IP or DNS name works too. The default port is 7443.

**Open the remote TUI from a shell.** Run the following in your terminal. At the first prompt, type the token from the computer you want to control and press Enter. Input is hidden. Successful pairing opens an interactive TUI on that computer.

```text
$ dima share connect <address>
Sharing token: _
    |
    +-- Enter --> remote TUI
```

**Run one remote CLI command.** Put the command after `--`. For example, `dima share connect <address> -- mcp list` prints the target's MCP configuration and returns to the shell. A previous successful pairing lets you run this without another token prompt. For a specific target directory, use `dima share connect <address> -- --cwd /srv/project mcp list`. To reopen a known session in the TUI, use `dima share connect <address> -- --resume <session-id>`.

Use `--no-remember` for a one-time pairing. After a token reset, enter the new one with `dima share connect <address> --pair`. Resetting the token or turning sharing off disconnects existing remote terminals. Remote access in Settings always controls the computer at the keyboard.

Run offline database maintenance (including `daemon vacuum` and raw Timeline `export`), migration, client updates, Web installation and local sharing settings directly on the intended computer. For a remote session export, use `dima share connect <address> -- session export <session-id> --format json`.

For clients using the shared byte stream directly, keep a Windows RPC open until its reply has arrived. Ending input closes that target named pipe in both directions. On a Windows keyboard computer, the proxy delivers the final reply and closes its local RPC pipe when the target stream ends. POSIX sockets and the TCP UI return stream can close their write side and continue reading.
