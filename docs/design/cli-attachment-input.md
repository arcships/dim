# CLI attachment input

Status: client implementation covers multiline/paste drafts, native clipboard/file insertion,
completion, dropped paths and headless images. Platform acceptance is recorded below. The server interfaces are implemented:
`attachment.put` (#102), image content blocks in
`session.submit`, and ModelProvider's image handling (#92). This document defines the remaining
CLI attachment work assigned in [roadmap #114](https://github.com/arcships/dim/issues/114).

The [Attachments client contract](plugins/Attachments.md#a3-cli-contract) and
[feature list](../feature-list.md#input-references-and-attachments) define the required sources:
clipboard images/files, `@` file selection, pasted/dropped paths, multiline and large text,
mixed attachments, marker removal and headless images. All enter through existing public RPC.
The [native CLI contract](../../crates/dim-cli/README.md) describes the executable. The current
editor holds ordered text, paste, file-reference and image items.

## Ownership and entry points

The CLI owns one local draft for the idle prompt. It captures local files and clipboard data
only after an insertion action. Text attachments remain SessionStore files, images become
ordinary message parts, and the server retains submission, adoption and model-capability policy.

The draft is an ordered sequence of text, file-reference, image and paste items. Each marker
represents its item's data rather than a substring to parse later. Typing text resembling a
marker creates ordinary text. The draft has no persisted identity or client-side send queue.
The existing run/approval loop takes keyboard ownership after a successful submission.
Account, OAuth and approval prompts retain their existing plain or hidden input behavior.

Interactive file paths use the CLI's selected local `--cwd` as their base, including after
resume. The composer displays that base. It does not obtain local files relative to a daemon's
storage path. Headless `--image` arguments use the invoking process's current directory, like
ordinary command-line file arguments. Paths returned by `attachment.put` are already paths on
the Session's resource node; the CLI uses them directly without local path resolution.

## Scrollback layout

```text
local files: /work/project
session: <session-id>

> Compare [Image 1: screen.png] with @docs/
  docs/guide.md
  docs/reference/
  Tab: select or enter directory   Esc: close suggestions

> Compare [Image 1: screen.png] with [File: docs/guide.md]
  Explain this excerpt: [Paste 2: 120 lines]
  Enter: send   Ctrl+J: newline   Ctrl+V: clipboard

> Compare [Image 1: first.png], [Image 2: second.png]
  and [Image 3: third.png]
  Error: the combined request exceeds 16 MiB after JSON encoding.
  Draft retained. Remove an image or shorten the message to continue.

> Summarize [Paste 1: 120 lines]
  Saving text attachment...
```

The composer stays in terminal scrollback. It redraws its own input and suggestion rows and
keeps the cursor in the draft. Image pixels are not rendered in the terminal. Markers show
escaped file names or paste line counts; labels are presentation, not model content.
After submission, transcript messages show image blocks as numbered `[Image #N]` markers
on separate lines in content order, including image-only messages and messages restored
from history. These fixed tokens do not change with the interface language. Numbering starts
at 1 for each message; the submitted image block has no filename. Tool results retain their
existing text-only display. Editing, quoting, branching, forking and withdrawing a queued
message reuse only its text; display markers are never inserted into those drafts.

```text
┃ What is this?
┃ [Image #1]
┃  [Pasted text: /files/paste.txt (1 lines)]

┃ [Image #1]
```

Suggestions show the current directory's matching entries; `Tab` enters a selected directory
or inserts a selected file. Up/Down selects a suggestion; Esc leaves its text in the draft.
Typing path components narrows the search from the displayed base. Suggestions list UTF-8 names;
the selected source must be an ordinary file before it can be inserted.
Selection never submits the draft. A missing or unreadable directory produces an inline error.

Text wraps at the available width; long labels truncate for display without changing paths.
When the draft exceeds the available rows, the input scrolls to keep its cursor visible and
leaves room for status. Resize recomputes that viewport. Cursor movement and deletion respect
Unicode grapheme boundaries and display widths. Terminal control characters are escaped at
display, while the submitted text and file bytes retain their original values.

## Editing and insertion

| Action | Result |
| --- | --- |
| Enter while idle | Validate and submit the complete draft, including an image-only draft. An empty draft does nothing. |
| `Ctrl+J`, or supported `Shift+Enter` | Insert a newline. |
| Arrows, Home/End, Backspace/Delete | Edit text; Home/End use logical line boundaries. A marker is one indivisible item and is removed as a whole. |
| `@` followed by a path query | Show local file suggestions at the cursor. Esc closes suggestions before acting on the draft. |
| Terminal paste | Insert at the cursor as one paste action when the terminal reports its boundaries. Embedded newlines do not submit. |
| `Ctrl+V` reaching the CLI | Read clipboard file entries, image data or text and insert them at the cursor. |
| `Ctrl+C` with an idle nonempty draft | Clear the draft and release its local image data. With an empty draft, use the existing exit behavior. |
| `Ctrl+C` during a running turn | Keep the existing stop and settlement flow. |
| `Ctrl+C` twice during terminal paste | Cancel the unfinished paste and retain the preceding draft, including when its closing delimiter is missing. This consecutive pair is reserved; a single Ctrl+C byte is literal paste content. |
| EOF key | Exit only with an empty draft; otherwise keep editing. Ctrl+Z is an EOF key only on Windows. |

Enable bracketed paste only while the composer owns input, and restore terminal modes and
paste settings when it yields or exits. The Unix input path must receive paste bytes before
the canonical line buffer can truncate them; raising the current 1 MiB line cap alone does not
implement this contract. Windows input must distinguish insertion/paste from submission too.
No key-name assumption makes unsupported terminal events available: if `Shift+Enter` is not
distinguishable, `Ctrl+J` remains the newline action. Without paste-boundary reporting, use an
explicit clipboard insertion for multiline text; do not infer paste boundaries from timing.

A paste consisting entirely of existing file paths is imported as files. Recognize whitespace
separators outside single/double quotes, backslash-escaped spaces/quotes, and local `file://`
URLs with percent-encoded paths. A single existing path containing spaces also works. This is
path parsing, without shell evaluation, variable expansion, globbing or implicit `~` expansion.
For a file URL, accept an empty authority or `localhost`; other URL authorities are not local
path imports. On macOS, resolve file-reference URLs and `/.file/id=…` paths with the
native URL API before classifying an image or constructing a file link. Accepted file items
carry image bytes or a real path, never the virtual file-reference ID. Linux and Windows
keep their native filesystem paths. File paths must be representable in UTF-8 without
lossy replacement.
If the entire payload is not a file list, retain it as text rather than extracting guessed paths
from prose. HTTP image links remain text; the CLI never fetches them as images.

File selection, clipboard file entries and path imports use the same insertion behavior:
supported image magic bytes produce image items; other ordinary files produce path-reference
items. Image intent with malformed or unsupported image data is rejected, preserving the
draft. A supported image's magic bytes determine its media type even if its filename disagrees.
Inserting a group is atomic locally: if a selected image fails validation, none of that group's
items are inserted. Removing an item never deletes a user's original file.

A multiline paste, or a paste over 16 KiB of UTF-8, is displayed as one compact marker.
Pastes of at most 16 KiB expand to their original text on submission. Larger pastes use
`attachment.put` only when the user submits. The threshold is one fixed CLI behavior, not a
token estimate or a new server setting. Preserve the delivered text, including CRLF and trailing
newlines. Count lines as Rust `str::lines()` does: a trailing newline adds no extra empty line.

### Terminal UI composer

The terminal UI (scrollback and fullscreen) follows the TypeScript TUI's markers, which differ
from the line client's above:

- **Dropped or pasted paths.** A terminal delivers a drag as a paste of the path. A paste naming
  only existing files or directories (one path or several per line, one line per path or group)
  inserts one atomic `[name]` marker per path; a repeated name reads `[name #2]`. A backslash
  escapes any character on Unix, as a shell's drag writes `\(` or `\ `. Into a shell-mode draft the
  paste stays text.
- **Clipboard images** read `[Image #N]`, counting the draft's images.
- **Pasted text.** A paste of fewer than 4 lines and 201 characters is typed into the draft. A
  longer one is one atomic `[... N lines]` marker (over 5,000 characters `[Text: preview (N
  chars)]`), and on submission it is stored with `attachment.put` and sent as
  `[Pasted text: <path> (<lines> lines)]`, whatever its size: what the composer collapses, the
  model reads as a file. The line client's 16 KiB rule above does not apply here.
- **`@` mentions.** `@` at the start of the draft or after a space or punctuation opens a file
  picker for the path query before the cursor. Up/Down select (wrapping), Enter or Tab accepts,
  Esc closes it for that query. A directory is typed in as `@dir/` and the picker stays open; a
  file becomes an atomic `@relative/path` marker followed by a space. `~/`, absolute and `../`
  prefixes list that directory. Hidden entries are offered only when the query names one.
- **Submission.** A file or directory marker becomes a Markdown link to its absolute path
  (`[name](/abs/path)`, `[@src/a.rs](/abs/src/a.rs)`); its bytes are not uploaded, and the model
  reads it with its tools. An image file is read and checked (A.1) at submission and sent as an
  image block only when the session's model takes image input: its `provider.catalog` entry has
  `vision`, or `image` among its `modalities`. Otherwise the image is sent as its link. A
  clipboard image has no path, so with such a model the draft is kept with the reason. A catalog
  that does not answer within 5 seconds leaves images sent as images. Audio and video files are
  sent as links: user input has no audio or video content block.

The `@` index must not slow the interface, whatever directory the CLI runs in. A project
directory is walked once on a thread of its own, breadth first, honouring `.gitignore` files and
skipping dependency and build directories (`node_modules`, `target`, `.git`, `Library` and the
like), within 8 levels, 50,000 entries and 2 seconds. The picker ranks what has been found so far
and redraws as more arrives; a finished index is reused for 10 seconds. The home directory and
the filesystem root are never walked: there the picker lists only the directory being typed, one
directory read of at most 2,000 entries, as it does for an outside prefix.

## Terminal and shell paste entry points

Terminal.app, iTerm2, Ghostty, Warp and VS Code handle their own paste shortcuts. A terminal's
`Cmd+V` or paste action supplies only the text or paths that host forwards; use the `Ctrl+V`
entry above for native clipboard images and files when the host forwards that key. The CLI
cannot observe a shortcut consumed without sending input. PowerShell and CMD are shells;
their terminal host determines which keys and paste events reach the CLI. Linux clipboard
backends and native platform validation are specified below.

Relative pasted paths follow the local `--cwd` contract above, independent of the shell.
Quoting a path does not run PowerShell, CMD or a Unix shell. Native execution is required
for platform acceptance; shared parser and emulated terminal tests alone do not establish
behavior in every terminal.

## Clipboard and local image validation

For one clipboard insertion, prefer the native file list when available, then an image, then
text; do not insert the same clipboard content through several representations. Native clipboard
reads refer to the machine running the CLI. SSH does not confer access to the user's other desktop
by itself; only the terminal can grant it, as below.
When the terminal intercepts `Ctrl+V`, its ordinary paste event follows the terminal-paste path.
If the OS clipboard component is unavailable, show that error and keep the draft. OSC 52 text
copying does not provide an image/file clipboard-read channel.

The terminal's own clipboard is read through the kitty clipboard protocol (OSC 5522) instead of
the native readers when both hold: the terminal reported mode 5522 to the startup DECRQM probe
(sent before the DA1 sentinel, so a terminal without it is detected as such and keeps the native
path), and the native readers cannot see that clipboard — the CLI runs over SSH
(`SSH_CONNECTION` or `SSH_TTY`), or on Linux without `DISPLAY` and `WAYLAND_DISPLAY`. Inside
tmux or screen the probe is answered by the multiplexer and the native path stays. The read first
lists the clipboard's types, which the terminal answers without asking, then requests one: an
image (PNG, JPEG, WebP or GIF, in that order) before text; other image types, such as TIFF, are skipped so
text listed beside them is still inserted. File lists are not read this way, as their paths name the terminal's machine. The terminal may ask the user to allow the
read; the request carries the name `dim` and a password fixed for the process, so a terminal that
remembers the answer asks once per run. A denial, an error status, an empty choice or no answer
within 60 seconds shows the error and keeps the draft; replies to an earlier, abandoned read are
ignored by their request id, and a reply that stops mid-sequence for 5 seconds is dropped so keys
are read again. Image and text size limits are those of the native readers.

macOS uses AppKit's pasteboard; Windows uses Win32 clipboard formats. Linux uses `wl-paste`
when `WAYLAND_DISPLAY` is set and otherwise `xclip` with the CLIPBOARD selection. On macOS the
copy path also hands the text to `pbcopy`, for terminals that do not take OSC 52; `pbcopy`
decodes its stdin in the locale's encoding, so a non-ASCII copy needs a UTF-8 locale (the CLI
sets `LANG`). These Linux utilities must be installed on the CLI's machine; they are not bundled
or installed automatically.
All are read only after Ctrl+V. No shell evaluates pasted paths or clipboard data. The Linux
reader is the editor's owned, cancellable subprocess; Ctrl+C clears the draft and releases it.
On Linux, editing remains available during the read, and the result inserts at the current
cursor. Enter while that read is pending asks the user to wait without submitting input.
macOS and Windows clipboard reads, pixel decoding and PNG encoding run synchronously;
input editing and cancellation resume when the read returns. A Linux `text/uri-list`
containing no `file:` URI falls through to image/text selection. A file list mixing local
files with other URIs is rejected atomically.

Read and validate image files at insertion so an accepted item retains the captured bytes even
if the original changes later. Clipboard pixel data is encoded once as PNG, after checking its
dimensions; it is not resized. An encoded image from the clipboard keeps its supported format.
No temporary image file is needed when the clipboard component can return bytes directly.
Any temporary file a platform component requires belongs to this draft and is removed on item
removal, successful submission or exit. Original source files are never temporary cache files.

Apply [Attachments A.1](plugins/Attachments.md#a1-image-handling-cli-checks-and-modelprovider)
at the local-data boundary: PNG, JPEG, WebP and GIF; at most 20 images per submission; at most
5 MiB per image file after base64 decoding; at most 8000 px per side. Reject invalid/truncated
headers, unsupported image data and violations rather than resizing. The 5 MiB limit is encoded
image-file bytes, not an assertion about expanded pixel memory. A paste is limited to 10 MiB
of UTF-8. The complete encoded request must fit the existing 16 MiB transport frame, including
its JSON envelope and escaping and excluding the NDJSON newline. Several individually valid
images can still exceed that combined request limit.

## Submission, rejection and cancellation

```text
idle draft
   |
   Enter
   v
local validation --> rejection: show reason, retain draft
   |
   v
attachment.put for each large paste, in item order
   |                |
   |                +--> rejection: retain draft and confirmed file paths
   v
session.submit with the ordered text/image parts
   |                |
   |                +--> uncertain error: diagnose and exit; no resubmission
   v
accepted reply --> release local images, clear draft, enter existing run loop
```

Validate all local items before starting file puts. Every put and submit uses the existing
`Client::send` encoding boundary, which must check the complete encoded request before writing
any bytes. JSON escaping can make a valid-size paste's put too large. The final submit is checked
after confirmed paste paths are available; puts and submission are not one atomic server action.

For each large paste, call `attachment.put {sessionId, text}` and use the typed reply's path
directly. Keep that confirmed path with the unchanged draft item for a later explicit attempt
in the same Session; do not put it again merely because another item or final request encoding
was rejected before submission.
Insert `[Pasted text: <path> (<lines> lines)]` at the item's position. An ordinary file reference
inserts its absolute path as text, and an image inserts the existing `ContentBlock::Image` with
base64 `ImageSource`. Text, references, paste expansions and images retain their relative order.
Selecting a file through `@` or a dropped path lets the model read that file through the
existing `read` tool, including files outside the workspace; selection does not upload its bytes.
No display marker id enters the model input. Use `UserInput.content` (`Vec<ContentBlock>`, wire
`user.content`) for mixed parts; the existing plain-text submission remains usable for text-only
input. Model-provider `MessagePart` is not the submission type.

Errors show the offending item and a bounded reason without dumping image data or paste content.
Local validation and rejected attachment puts keep the draft editable. `unknown_method` from
`attachment.put` means the host does not offer text attachments; retain the paste and report
that limitation. Do not silently change a large paste to inline model text. A confirmed stored
file whose draft item is removed remains SessionStore data and is cleaned with its owning
Sessions; no attachment deletion method or rollback protocol is added.

If interrupted before any request is sent, stop preparation and retain the draft. If a put is
already sent, drain its reply, keep any confirmed path, and send no further put or submission.
An already sent submission retains the existing stop and settlement behavior. A `session.submit`
error reply is not proof of rejection: v3 can commit the input and then return
`submission_unknown` or a publication failure on the same connection. Keep the existing
Session-handle diagnostic and exit behavior for these errors instead of offering a new send.
Connection loss can also leave a put or submission outcome unknown and uses that same exit
behavior. No automatic retry, lookup-based recovery or persistent local recovery store is added.
Only a proven pre-commit rejection is safely resubmittable. For v3 `session.submit`,
`error.data.appError` values `invalid_params`, `session_invalid_input`, `session_closed`,
`session_build_failed`, `service_reloading` and `unknown_method` are the server's explicit
[pre-commit guarantee](../../crates/dim-app-server/README.md#session-submission-errors).
They retain their ordinary error diagnostics; every other error, including `submission_unknown`
and opaque publication/storage failures, retains the uncertain-outcome exit without resending.
This does not promise that an unchanged rejected request will succeed or add automatic retry.
Draft retention does not promise recovery after process exit.

Recognized CLI commands run only from a plain single-line draft. Commands are not obtained by
expanding paste or attachment markers. A recognized command with attached items reports a usage
error and retains them before any put. Existing command handlers retain their own parsing and
RPC contracts; headless slash commands remain literal model text.

## Headless images

`dimcode exec [resume SESSION_ID] [--image PATH ...] [-p TEXT] [--json]` accepts repeated image
arguments and image-only requests. When text is supplied through `-p` or redirected stdin, its
text part comes first, followed by images in argument order. Relative image paths use the
invoking process's cwd. Apply the same image checks and encoded-frame limit before submission;
`--image` accepts local files, not URL sources. Invalid input reports an error and a nonzero exit.
Headless execution does not inspect the clipboard or infer images/file references from text.
Its notification output and approval/stop semantics remain the existing exec contract.

```text
$ dimcode exec --image before.png --image after.png -p 'Explain the change'
$ dimcode exec resume <session-id> --image screenshot.png --json > run.jsonl
$ printf '%s\n' 'Describe the screenshot' | dimcode exec --image screenshot.png
```

## Impact and verification

Multiline and large pastes use bracketed boundaries, atomic markers, submit-time
`attachment.put` and draft retention. Headless images use `--image`. The encoded-request guard
in #153 applies at the shared client boundary. Nonempty-draft Ctrl+C clears the draft.

Native clipboard images/files, `@` completion and dropped paths enter the same draft.
Platform acceptance records native calibration and dependencies. No clipboard wrapper library is required:
`objc2` and the Foundation/AppKit bindings (MIT) bind macOS, and the existing `windows-sys`
dependency (MIT OR Apache-2.0) binds Windows. Linux invokes the installed system component.
The existing image codec enables TIFF for macOS pixel data and BMP for Windows DIB data only;
submitted image formats remain PNG, JPEG, WebP and GIF. The existing `url` crate parses local
file URLs. Neither encoded clipboard images nor selected image files are re-encoded.

| Area | Required client work |
| --- | --- |
| Input and rendering | Idle draft editor, atomic markers, path suggestions, paste events and terminal-mode restoration. Reuse the existing plain/hidden prompts outside the composer. |
| Arguments | Repeated exec `--image` paths and image-only execution. Preserve text-only and JSONL interfaces. |
| Local data boundary | Clipboard/file reads, image headers/dimensions, base64 and exact UTF-8 paste contents. Select platform libraries here rather than adding agent services. |
| Session flow | Existing create/attach/submit/idle/detach plus C1 `attachment.put`; preserve admitted request ownership and terminal filtering. |
| Transport | Check the existing 16 MiB limit at the complete request encoding boundary; follow-up #153 implements that guard. |
| Documentation and tests | Native CLI contract/help and existing public `cli_flow.rs`; actual disposable App Server flow for stored text and image content. |

Use the existing Unix PTY flow for typed multiline input, split paste delimiters, cursor/marker
deletion, suggestions, Unicode display, resize, draft retention and terminal restoration.
Exercise the actual clipboard component on each supported OS, plus unavailable-component behavior;
a mocked clipboard alone does not establish native image/file support. Verify PNG/JPEG/WebP/GIF,
mislabeled files, invalid headers, bounds, mixed order and headless images through public history
and a scripted model request. Verify a stored large paste is read by the model through the real
`read` tool, and a rejected/removed item contributes no submitted content. Check JSON escaping
at the frame boundary, put rejection/cancellation, a confirmed put followed by submit rejection,
and connection loss without resubmission. Reuse existing approval, exec and stop evidence where
their paths remain unchanged. Include a committed input followed by a publication error reply
without disconnect: the CLI reports uncertainty and exits without sending a second submission.
All homes and images are disposable. Native clipboard calibration
uses an isolated desktop session with fixture clipboard contents.

Headless image flows cover supported formats, byte/order preservation, boundaries, local path
resolution, combined encoded frames and uncertain error replies without resubmission. The
existing actual CLI/App Server model flow checks the image in the model HTTP request and stored
history after resume. The paste flows cover literal CRLF, Unicode grapheme editing, atomic
removal, command rejection before storage, UTF-8 and size limits, put rejection, interruption
and drain, confirmed-path reuse and local encoded-frame rejection. The actual App Server/model
flow stores a paste and reads its exact file through the real read tool.

Native tmux calibration on macOS covers wrapped/multiline input, bracketed paste and viewport
changes at 80x24, 40x10 and 15x6. The renderer accounts for previously painted lines reflowing
when terminal width changes. Unix input owns a separately opened descriptor for the inherited
terminal, so nonblocking reads cannot change stdout's status flags. On macOS it names that
descriptor with `F_GETPATH`: `ttyname_r` scans `/dev` and can miss this terminal while another
terminal's entry is removed. The CLI flow also verifies
complete model output while a terminal reader pauses. Windows input uses virtual-terminal mode with the existing console
record reader. Its ConPTY flow exercises Unicode/grapheme editing, arrow/delete input,
bracketed paste boundaries and raw Ctrl+C clearing through the real console. It reuses the
workspace's portable-pty version as a Windows-only development dependency. Native clipboard checks
use a unique AppKit pasteboard and a private Windows window station.
The Windows boundary flow runs on the disposable Windows CI job; its execution must pass before
claiming that platform's clipboard acceptance. Linux runtime acceptance with actual `xclip`
and `wl-paste` remains pending a recorded manual run in isolated Xvfb/X11 and headless
Sway/Wayland sessions, including date, commit and utility versions. The required cases are
PNG, file lists, exact text, rejected/oversized input and unavailable data. Linux binary flows
using a fixture `wl-paste` cover URI selection, raw-key cancellation, reader reaping and source
failure without losing the draft; they do not establish native desktop acceptance. The
production App Server/model/history flow also submits selected images and a file reference
outside the Session workspace, then reads that file through the real tool. Wayland and Windows
runtime results must be reported separately from compilation. No App Server, core, protocol, assembly, persisted format or
capability change is required. No mechanism requirement is raised.

## Rule check and ablation

| Rule | Application |
| --- | --- |
| §4 rules 1–2 | SessionStore owns files and the existing submission/adoption flow owns accepted input; the CLI changes neither owner's facts. |
| §4 rule 3 | Validation is at the actual local-data and transport boundary; no before callback. |
| §4 rules 4–5 | Existing RPC and commit behavior; no core branch or atomic batch extension. |
| §4 rule 6 | Only the ordinary adopted input or a later `read` tool result enters model context. |
| §4 rule 7 | Existing public Session RPC and local OS APIs; no private plugin state. |
| §4 rule 8 | No infrastructure requirement. The real canonical-input limit is handled inside the CLI editor. |
| §4 rule 9 | Local draft/presentation stays out of Timeline; only submitted content is sent. |
| §1 | This is client interaction, not an agent Capability; App Server keeps its existing responsibilities. |
| ADR 002 D1–D3 | Draft and temporary resources belong to the CLI; existing Session services are called normally. No wiring plugin or core dependency on attachments. |
| ADR 002 D4–D5 | No new loader/business configuration or language-specific server contract. Existing DTOs and errors are consumed directly. |

The draft editor is necessary to retain attachments after rejection and receive large paste
bytes without canonical-buffer truncation. Item data is necessary for ordered submission and
atomic marker removal. A confirmed paste path is the actual result of a completed put, not a
second upload registry. Temporary files, a client attachment store, preview RPC, upload manager,
command registry, image-conversion service and new cancellation protocol are unnecessary.
The editor keeps only the pending clipboard I/O future, so its existing raw-key loop can cancel
the read; it starts no reader task or manager. Completion entries carry their path, label and
directory flag from the filesystem listing rather than re-reading presentation fields.
