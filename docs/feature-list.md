# CLI feature list

This list describes target capabilities and interfaces for `dimcode`, not implementation status. The npm launcher also exposes the `dim` alias.

The CLI owns terminal and command interactions; the App Server owns host configuration, external protocols and persisted Sessions; Agent v4 Capabilities own branch execution. The [architecture](architecture/README.md) and [foundation ADR](adr/001-harness-foundation.md) define the existing contracts. Provider-specific account behavior belongs to separately maintained plugins under the [managed provider boundary](architecture/development.md#managed-provider-boundary).

## Providers and models

- OAuth Providers: sign in, sign out and refresh account state.
- API-key Providers: configure API keys, service URLs and models.
- Provider management: add, edit, test, enable, disable, remove and switch Providers.
- `/connect`: connect and manage Providers; alias `/provider`. The Dim account is not listed here.
- `/login`: sign in to the Dim account (managed provider) by browser, enterprise email or device code; `/logout` signs it out.
- `/models`: view and switch models, and adjust supported reasoning effort.
- `/effort`: adjust the current model's reasoning effort; alias `/reasoning`. Fullscreen TUI has a separate entry; scrollback adjusts effort through `/models`.
- `/fast`: toggle fast mode when supported by the current model.
- Subagent models: inherit the main session's model, use a fixed model or let the Agent choose.
- Auxiliary models: configure models for auxiliary tasks.

## Session management

- `/new`: start over on an empty screen; the first message creates the Session.
- `/resume`: view and resume historical Sessions; alias `/sessions`.
- `/export`: export conversation history as Markdown or JSON.
- `/timeline`: list the shown line's user messages, jump to one, and edit, quote, branch or fork from it.
- Session list: view Sessions for the current project or all projects.
- Session titles: generate titles automatically; support manual renaming through the scrollback panel and CLI commands.
- Session forks: create a new Session from a historical message.
- File rollback: roll back file changes recorded by the Session; fullscreen TUI only.
- File and conversation rollback: rewind files and the conversation to a historical point together; fullscreen TUI only.
- Session deletion and cleanup: delete Sessions through CLI commands; clean up by age or retained count, with a preview.
- Request snapshot export: export model request context through CLI commands, with filters for reasoning, tool records and compressed content.

## In-session interaction

### Sending, queuing and steering

- Send: press Enter while idle to submit input and start the Agent; combine text, file references and images in one message.
- Queue: messages submitted during an Agent run enter the queue without immediately interrupting the current task.
- Automatic sending: after the current run ends, send queued messages in submission order to start subsequent runs.
- Steer: add a queued message to the current run's context at a subsequent Agent turn boundary without stopping the task first.
- `Ctrl+S`: steer the earliest queued message into the current run. This does not submit the current draft; an empty queue has no effect.
- `/queue`: view messages awaiting submission and messages being incorporated into the current run; select an entry to steer or delete it.
- Retrieve for editing: with an empty draft, press Up to move the latest queued message back into the input area. Messages with attachments or file references cannot be retrieved this way; use `/queue` to manage them.
- Queue deletion: remove messages that have not been sent; a steering instruction already handed to the current run cannot be deleted as a queued message.
- Interrupt: press Esc to stop the current Agent run; an open dialog or input suggestion takes priority.
- Clear draft and exit: while idle, clear the draft or exit the terminal. During a run, the first `Ctrl+C` interrupts in scrollback; fullscreen TUI clears the draft or enters the exit flow according to the input state.

`send`, `queue` and `steer` name submission behaviors. There are no `/send` or `/steer` commands.

### Input, references and attachments

- Multiline editing: use `Shift+Enter` or `Ctrl+J` for a newline; `Shift+Enter` requires terminal support.
- Input history: use Up/Down to retrieve submitted prompts and `!` / `!!` command history, stored per working directory; availability depends on cursor position and draft state.
- Command completion: use `/` to find commands and Skills; execute a selected command or complete it before entering arguments.
- Skill references: choose a Skill through `$`, `/` or `@` suggestions and insert `$skill: <name>` for submission with the message. Inserting a reference does not send the message.
- File references: use `@` to search project paths, browse directories and select files; ordinary files are passed as paths for the Agent to read as needed.
- Pasted and dropped paths: recognize existing paths, including multiple paths, quoted paths and `file://` URLs. Ordinary files become path references; supported images become attachments. Drag-and-drop depends on the terminal passing paths.
- Multiline paste: display long or multiline text as a compact block and restore the full text on submission.
- Large paste: turn very large text into a text attachment; save the full text on submission and provide a reference instead of inserting it directly into the prompt body.
- Image insertion: attach clipboard images, image paths or images selected through `@`. Accept PNG, JPEG, WebP and GIF; image understanding requires model support.
- Mixed attachments: combine multiple images, text attachments, file references and explanatory text in one message.
- Remove references and attachments: deleting an input marker excludes it from submission. Removing an image attachment cleans up terminal-created temporary cache files without deleting the user's original file.
- `!command`: execute Shell commands in the Session and include their output in subsequent model context.
- `!!command`: execute Shell commands without adding their output to model context; retain the terminal record.

### Clipboard and message viewing

- Text paste: use the terminal's paste shortcut to insert at the cursor; recognize ordinary text, multiline text, large text and file paths.
- Clipboard images: use `Ctrl+V` to read screenshots or images from the system clipboard and insert attachments.
- Clipboard files: use `Ctrl+V` to read files copied in a file manager; images become attachments and other files become path references, including multiple files.
- Message copying: copy message text to the clipboard through message actions.
- Selection copying: fullscreen TUI supports mouse selection and copies selected text on release; scrollback uses the terminal's native selection and copying.
- Clipboard compatibility: copy text through the system clipboard and OSC 52 for local and SSH use. Over SSH, terminals with the kitty clipboard protocol (OSC 5522, e.g. kitty, Ghostty 1.4+) also serve `Ctrl+V` image (PNG, JPEG, WebP, GIF) and text reads from the local clipboard; other terminals keep the native readers. Image/file reads and remote copying depend on platform components and terminal support.
- History browsing and navigation: use `/timeline` to locate messages. Fullscreen TUI loads earlier records from a row at the top of the transcript; in scrollback a `/timeline` jump to an earlier message draws history from it, and the terminal's own scrollback does the rest.
- Open links: click web and local-file links in fullscreen TUI output to open them with the system's default browser or application.
- Tool groups: each group of tool calls is collapsed by default (`▸`); click its header in fullscreen TUI to expand or collapse that group. `/tools` expands or collapses every group.

### Run feedback and confirmation

- Streaming output: display Agent replies, model-provided reasoning previews and tool results as they arrive. Reasoning has a single-line preview without a separate full-text expansion entry.
- Run status: show the current task, tool status and errors; scrollback also shows elapsed run and tool-wait time.
- Terminal status: tell the terminal whether dim is idle, working, waiting on a permission or a question, done or failed (OSC 7501), in both terminal UI modes, so tabs and inboxes can show it; `DIM_TERMINAL_STATUS=off` turns it off.
- Terminal progress bar: an indeterminate bar while a run works and a paused one while it waits on a permission or a question (OSC 9;4), in both terminal UI modes, on Windows Terminal, ConEmu, Ghostty, WezTerm and iTerm2 3.6.6+; `DIM_TERMINAL_PROGRESS=off` turns it off and `on` draws it in any terminal.
- File change feedback: show write/edit results and diff summaries.
- Tool approvals: allow or deny operations during a run, with one-time authorization and remembered permission decisions.
- User questions: answer Agent questions with single choice, multiple choice, multiple questions and custom answers.
- Todo status: show the Agent's task breakdown, current item and completion state. Not planned ([overview](design/overview.md#1-scope-and-premises) §1).
- Background tasks: view background processes, subagents and workflows in the Session; open output or conversation details and stop individual or all tasks.

## System management

- `/exit`: exit terminal interaction.
- `/settings`: system and runtime settings.
  - Subagent model and reasoning effort.
  - Auxiliary model and reasoning effort.
  - Default multimodal Provider and model.
  - Execution Shell.
  - Tool approvals.
  - Sandbox.
  - Command execution permissions.
  - Mouse settings; fullscreen TUI only.
  - VS Code focus integration.
  - Resource monitoring.
  - Default terminal mode.
  - Task completion notifications.
  - Plan usage display.
  - ACP Session display.
  - Agent prompt language.
  - Web access settings.
- `/usage`: view context usage; also display plan quota and usage when supplied by an optional account Provider.
- Context use in the composer's bottom border, after the approvals mode (`1.8K / 256K`); refreshed when a run on the main branch ends.
- `/stats`: view Session statistics and activity; fullscreen TUI only; alias `/state`.
- `/language`: switch interface language; aliases `/lang` and `/locale`.
- `/theme`: switch themes.
- `/keys`: keyboard shortcut help; scrollback only.
- `/debug`: model-run diagnostics.
- `/tui`: switch between fullscreen TUI and scrollback while preserving the current Session and running tasks.
- Version and upgrades: view the version, check for upgrades and self-update.
- Debug logs: configure logging and runtime tracing.

## Branch Agent behavior

- `/agent`: switch Agent mode.
- `/goal`: switch Goal mode; set, edit, pause, resume and clear a goal.
- Goal continuation: continue toward an explicit goal and record completion or blockage.
- `/approvals`: manage tool approvals and permission rules.
- `/approval`: set the session's tool approval to ask or full access. Ask is workspace-write. The choice is remembered for new sessions.
- Permission policies: read-only, workspace-write and full access.
- Sandbox: configure file, network and process access; availability depends on the platform and runtime environment.
- Command rules: configure allowed commands and the execution Shell.
- `/compact`: manually compact the current Session context.
- Automatic compaction: compact long context and continue the task.
- `/scheduled`: view and manage scheduled tasks through the scrollback panel; alias `/schedule`.
- Scheduled tasks: create, edit, pause, resume, delete, run immediately and view execution history.
  - Configure recurrence, prompt, working directory, model and whether to create or reuse a Session.
  - Both fullscreen TUI and scrollback can manage tasks and drive scheduled execution through the `schedule` tool.
  - Automatic triggering requires the interactive terminal process to remain alive; skip missed windows without catch-up runs.

## Extensions

- `/plugins`: manage plugins; alias `/plugin`.
- `/reload-plugins`: reload plugins and plugin-provided Skills.
- Plugin marketplaces: search, view details and install plugins.
- Git plugin marketplaces: add, view, update and remove marketplaces.
- Plugin installation: install from a marketplace, local path or Git URL.
- `/skills`: view, invoke, install, remove, enable and disable Skills.
- `/reload-skills`: reload Skills.
- Skill sources: user directories, project directories, plugins and configured Skill directories.
- `plan` Skill: investigate a task and generate a Markdown implementation plan; it is not a separate runtime mode. Not planned ([overview](design/overview.md#1-scope-and-premises) §1).
- `AGENTS.md`: load project and global working conventions.
- `/mcp`: manage MCP services and tools.
- MCP connections: support local processes, HTTP and SSE, including testing, enablement, disablement and OAuth authorization.
- `/workflow`: view workflows, supply parameters and start execution.
- `/workflow:save`: save workflows for reuse.
- Workflow management: view stages, logs, status and results, and stop execution.
- `/external-agents`: manage external Agents; alias `/agents`.
- External Agents: integrate locally available Claude Code, Codex, Cursor, OpenCode, ZCode and similar tools.
- Parallel subagents: delegate tasks, choose models, control concurrency, view results and cancel tasks.
- Hooks: review, enable, disable or globally disable command Hooks.
- Web search and fetching: search information and read webpage text, with the required configuration or account permissions.
- Slack and Feishu: send messages and task completion notifications when the corresponding service is enabled and configured.

## Workspace tools

- `read`: read text files; vision models can read supported image formats.
- `write`: create or overwrite files.
- `edit`: make precise file-content changes.
- `glob`: find files by path pattern.
- `grep`: search file contents.
- `exec`: execute Shell commands, project scripts and tests, with background execution support.
- `wait`: wait for background tasks to complete or for user steering input.

## Remote and external access

- `/web`: open the Web workspace from the terminal.
- Web access: start the service, configure authentication and access permissions, and rotate access tokens.
- `/remote`: manage remote access, including restart.
- Remote service: start in the foreground or background, view status, stop, enable and disable.
- Phone pairing: connect through a QR code or pairing code, and view or revoke authorized devices.
- Remote Sessions: access projects and Sessions from a phone; collaborate with or take over from the terminal.
- ACP: connect editors and other clients through standard input/output.
- JSON-RPC: provide a persistent headless service with multiple Sessions and event subscriptions.
- VS Code: launch the integration and support window-focus behavior.

## CLI commands

- `dimcode` / `dimcode tui`: start the interactive terminal.
- `dimcode tui --fullscreen` / `dimcode tui --scrollback`: select the terminal mode.
- `dimcode tui --fullscreen default` / `dimcode tui --scrollback default`: set the default terminal mode without starting a Session.
- `dimcode tui --session <id>`: open a specified Session.
- `dimcode exec`: execute a headless task with stdin, images, model selection, permissions and prompt configuration.
- `dimcode exec resume`: resume an existing Session and continue execution.
- `dimcode exec --json`: emit a JSONL event stream.
- `dimcode exec --rpc`: start a persistent JSON-RPC service.
- `dimcode exec --verify <command> --repair-attempts <n>`: verify after execution and continue repairing on failure up to the configured attempt count.
- `dimcode acp`: start the ACP service.
- `dimcode provider`: manage Providers.
- `dimcode model`: view and refresh models, and configure subagent model policies.
- `dimcode auth`: sign in, sign out, refresh, query and repair account state.
- `dimcode session`: view, rename, export, delete and clean up Sessions.
- `dimcode export`: export a Session's whole Timeline, including every final model request ([ADR 005](adr/005-observability.md) D3).
- `dimcode plugin`: manage plugin installations and marketplaces.
- `dimcode mcp`: manage MCP services and authorization.
- `dimcode hooks`: review and manage command Hooks.
- `dimcode web`: start the Web workspace.
- `dimcode remote`: manage the Remote service and phone authorization.
- `dimcode usage`: query plan usage through an optional account Provider.
- `dimcode upgrade` / `dimcode update`: upgrade the CLI.
- `dimcode version`: view the version.
- `dimcode help`: command help.

## Multimodal

- `dimcode image generate`: generate images.
- `dimcode image edit`: edit with reference images and masks.
- `dimcode image enhance`: upscale, restore, denoise, sharpen, remove backgrounds and colorize.
- `dimcode image read`: understand image content.
- `dimcode modality`: view capabilities and configure default Providers and models.
- `dimcode modality task`: view tasks, query status, wait for completion and cancel; show only capabilities supported by the current build.
- The following are available only in specific Desktop builds and are outside the standalone CLI's default scope:
  - `dimcode video`: video generation, editing and enhancement.
  - `dimcode speech`: speech synthesis and transcription.
  - `dimcode music`: music generation.
  - `dimcode audio`: audio generation.
  - `dimcode asset`: 3D asset generation.
  - `dimcode motion`: motion-data generation.
  - `dimcode realtime`: realtime connections.
  - `dimcode ocr`: image and PDF text recognition; requires OCR runtime components.
