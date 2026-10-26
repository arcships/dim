# Dim

Dim is a Rust coding agent with a terminal scrollback interface.

Install the `dimcode` package with optional dependencies enabled, then run `dim` or `dimcode`.
The platform package contains the native CLI and App Server. The optional `dimcode-plugins` package holds Dim's TypeScript plugins (Goal, Web, SessionTitle, Workflow, Scheduler), which the App Server loads by default with this Node; it depends on the rutis Node runtime, whose `esbuild` dependency declares an install script. Installing with `--ignore-scripts` skips it, and the plugins still run. Without `dimcode-plugins` the App Server runs only the plugins compiled into it.

Use `dimcode --help` for configuration and session commands. Platform packages are built for macOS arm64/x64, Linux arm64/x64 and Windows arm64/x64. Availability depends on verified release artifacts.

TypeScript plugins can import named-service types with
`import type { PluginFiles, Settings, Sessions, AuxiliaryModel, ToolRegistry, SessionMethods } from 'dimcode/plugin'`.
See the [plugin author's guide](https://github.com/arcships/dim/blob/main/docs/typescript-plugin-authoring.md).
The type-only entry adds no runtime code or plugin registration.
