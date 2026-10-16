# CLI attachment input

Status: proposed client interaction contract. The native CLI currently accepts one line per
turn. The server interfaces are implemented: `attachment.put` (#102), image content blocks in
`session.submit`, and ModelProvider's image handling (#92). This document defines the remaining
CLI attachment work assigned in [roadmap #114](https://github.com/arcships/dim/issues/114).

The [Attachments client contract](plugins/Attachments.md#a3-cli-contract) and
[feature list](../feature-list.md#input-references-and-attachments) define the required sources:
clipboard images/files, `@` file selection, pasted/dropped paths, multiline and large text,
mixed attachments, marker removal and headless images. All enter through existing public RPC.
The [native CLI contract](../../crates/dim-cli/README.md) describes the executable; this proposal
does not establish an implemented editor or clipboard binding.

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
Suggestions show the current directory's matching entries; `Tab` enters a selected directory
or inserts a selected file. Typing path components narrows the search from the displayed base.
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
| Arrows, Home/End, Backspace/Delete | Edit text; a marker is one indivisible item and is removed as a whole. |
| `@` followed by a path query | Show local file suggestions at the cursor. Esc closes suggestions before acting on the draft. |
| Terminal paste | Insert at the cursor as one paste action when the terminal reports its boundaries. Embedded newlines do not submit. |
| `Ctrl+V` reaching the CLI | Read clipboard file entries, image data or text and insert them at the cursor. |
| `Ctrl+C` with an idle nonempty draft | Clear the draft and release its local image data. With an empty draft, use the existing exit behavior. |
| `Ctrl+C` during a running turn | Keep the existing stop and settlement flow. |

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
path imports. File paths must be representable in UTF-8 without lossy replacement.
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

## Clipboard and local image validation

For one clipboard insertion, prefer the native file list when available, then an image, then
text; do not insert the same clipboard content through several representations. Clipboard reads
refer to the machine running the CLI. SSH does not confer access to the user's other desktop.
When the terminal intercepts `Ctrl+V`, its ordinary paste event follows the terminal-paste path.
If the OS clipboard component is unavailable, show that error and keep the draft. OSC 52 text
copying does not provide an image/file clipboard-read channel.

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
Only a proven pre-commit rejection is safely resubmittable; an RPC error code alone does not
establish that guarantee. Draft retention does not promise recovery after process exit.

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

Implement multiline and large paste handling first: bracketed paste, atomic markers,
submit-time `attachment.put`, draft retention and headless `--image`. The encoded-request guard
in #153 applies at the shared client boundary. Add the nonempty-draft `Ctrl+C` clear behavior
to the CLI README and help with this implementation.

Native clipboard images/files, `@` completion and dropped-path imports follow separately.
Their implementation PR identifies any clipboard dependency and its license and records
native calibration on each supported platform. Completion of the first step does not imply
these insertion sources are available.

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

This document changes no executable; product tests have not been run for the proposed editor.
Implementation starts after design acceptance. No App Server, core, protocol, assembly, persisted
format or capability change is required. No mechanism requirement is raised.

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
