# Attachments

Attachments imports files for a Session and returns durable references. Its public
face is the `attachment` namespace contributed to
[`sessionMethods`](../contracts/session-boundary.md#a1-method-contributions).
The cross-module design is [file transfer](../research/file-transfer.md).

## 1. Structure

| Part | Scope | Consumes | Provides |
| --- | --- | --- | --- |
| Attachments | Session | Session identity; [`sessionFiles`](../appserver/session-files.md#9-interface); `sessionMethods` | `attachment.put`, `attachment.import`, `attachment.read` |

The profile installs Attachments where SessionStore and local execution are
available. Each method uses its own Session identity. Rpc authorizes and holds the
Session for the call. Import by path is admitted from the local transport or from
WebEntry's internal invocation after an authenticated HTTP upload.

## 2. Data and state

Attachments owns no durable state. SessionStore owns the accepted bytes, file IDs,
ownership, fork sharing and deletion. Its replies supply the path and byte count.
A reference becomes context only through adopted input or a tool result.

An image source is `{type:"file",mediaType,fileId}`. Its accepted bytes are immutable.
Image preparation may produce scaled bytes before import; those bytes receive a
new file ID. A file imported without a subsequent submission stays owned by the
Session and is released through Session deletion.

## 3. Flows

### 3.1 Image at submit

The client imports a local source, receives an image reference, then includes it
in `session.submit`. Web clients first upload the browser file through the
[Web HTTP API](../web-workspace.md#session-file-http-api). Rpc validates reference
shape and ownership. A failed import preserves the client's draft.

The per-request image view — pixels, a reference or a placeholder (A.1) — and the
final provider encoding are requirements to their owners in A.4. The context flow
is defined by the cross-module design.

### 3.2 Text paste

`attachment.put` stores UTF-8 text. A file upload may instead import its received
path. The client quotes the returned absolute path in user text:

```text
[Pasted text: /absolute/session-file-path (1200 lines)]
```

### 3.3 How the model reads the file

A text attachment's path enters adopted user input. The model may call `read`
subject to its enabled tools and permission policy. Session ownership satisfies
file scope under [Session files §11](../appserver/session-files.md#11-security).

## 4. Timing and conflicts

`put` and `import` use drain interruption. Cancellation before the owner's commit
cleans pending storage; after commit the stored result stands. Losing a client
connection does not revoke committed ownership. Retrying an import creates a new
file. `read` reports interruption and has no durable mutation.

## 5. Context mapping

Only adopted inputs and tool results introduce attachment content to context.
The consuming request selects its view from its model capability. Canonical
history retains file references through model changes. Compaction controls which
historical references remain in effective context.

## 6. Interfaces (summary)

| Method | Mutating | Interruption | Contract |
| --- | --- | --- | --- |
| `attachment.put` | yes | drain | A.2 |
| `attachment.import` | yes | drain | File import and read |
| `attachment.read` | no | report | File import and read |

## 7. Security

Source-path authorization belongs to the entry receiving the request. A browser
cannot supply a host source path through RPC. The importer accepts regular files
and applies the Session file size limit. Image preparation opens the source,
checks its length, and applies the shared image format, pixel and scaling rules
before committing ownership. On Unix its open is nonblocking and rejects a terminal
symlink. The destination and permissions are chosen by SessionStore.

Reads require the current Session's ownership of the file ID. A file ID or path
alone grants no access. Attachment methods are user actions under
[ADR 003](../../adr/003-approval-scope.md), not agent-selected tool calls.

## 8. Rule check

| Rule | Application |
| --- | --- |
| Capability / principles §1 | Attachments is a Session method provider; model request adaptation belongs to ModelProvider's Capability. |
| §4.1–2 | SessionStore owns file mutations. Adopted inputs and tool results perform context handover. |
| §4.3–5 | No before callback or Timeline mechanism change; asynchronous preparation runs outside the commit lock. |
| §4.6 | Only adopted content reaches model context. |
| §4.7–8 | Uses owner services; their contracts and raised requirements are linked below. |
| §4.9 | Method replies report import/read outcomes; no new Timeline fact is needed. |
| ADR 002 D1–D3 | Session-scoped methods, no wiring part; dependencies are declared by name. |
| ADR 002 D4–D5 | File and image budgets belong to their owners; requests and replies are data. |

## 9. Requirements raised to mechanism owners

| Owner | Requirement | Ordering impact |
| --- | --- | --- |
| SessionStore | [Path import and authorized bounded reads](../appserver/session-files.md#9-interface), using one owner through native and wire bindings | Reply after file and ownership commit; fork/delete preserve live ownership |
| WebEntry / Rpc | [HTTP reception](../web-workspace.md#session-file-http-api) and local-only path admission | Temporary file remains available through import settlement |
| ModelProvider / Aimux | A.4 and [request execution](../../architecture/rust-agent/plugins/ModelProvider.md#durable-images-at-model-execution) | Resolve images after effective-context and capability selection |

## Appendix A. Interface details

### A.1 Image handling (CLI checks and ModelProvider)

The accepted image formats and scaling rule are owned by
[WorkspaceTools: read results and images](../../architecture/rust-agent/plugins/WorkspaceTools.md#read-results-and-images).
Image preparation uses that same rule. A submission accepts at most 20 images.
Image bytes follow the Session file limit; the submission carries file IDs.

ModelProvider routes every image block in a conversational request, in user
content and tool results alike, by the request model's vision capability and
the resolved destination's format support. A vision-capable model, or a request
whose model capability is unknown, receives a sendable image as an image block
when the destination takes its format; a file-backed source is resolved to its
bytes and checked by the same rule before it is sent, so an empty owned file
takes the placeholder. A model without image input never receives pixels, and
an image in a format the destination does not take takes the reference or
placeholder. An image that cannot be sent — a URL source, a file without an id,
empty, missing or undecodable base64, or a format this build cannot decode —
becomes the omission placeholder:
`[Image omitted: the current model does not accept images.]` for a model
without image input, otherwise `[Image omitted: the image cannot be sent to this model.]`.

When the pixels are not sent, a decodable image becomes `img:` followed by the
first 16 lowercase hexadecimal characters of SHA-256 of the base64-decoded
image file bytes, not the pixels or base64 text. Equal bytes have equal
references; the reference contract is [ImageUnderstanding §3](ImageUnderstanding.md#3-references).
File-backed sources must be resolved before computing the reference.

The destination's format support is part of that routing rule: one whitelist
per protocol, and for the compatible protocol the vendor package the connection
names. The preparation formats are PNG, JPEG, WebP and GIF. Gemini takes all but
GIF (its guides list PNG, JPEG, WebP, HEIC and HEIF), so a GIF becomes its
reference there; every xAI destination takes JPEG and PNG only (the Chat
Completions and Responses image input limits) — a native xAI route by its
protocol and the built-in entries that serve xAI over the compatible protocol by
the vendor package their connections carry — so a WebP or a GIF becomes its
reference there; and every other destination takes the four a request may carry,
as a destination whose protocol is unknown does. A standalone image
question follows the same rule and fails instead of sending
([ImageUnderstanding §3](ImageUnderstanding.md#3-references)). The provider
request encoder applies the destination's message-placement support: Bedrock
takes an image inside a `toolResult` block only on Amazon Nova and Anthropic
Claude 3 and 4, and AWS supports that block's `status` field only on the same
families; on the other Bedrock models the image follows the result in a user
message, and a failed result's text starts with
`[Error: this tool result reports a failed tool call.]` because that wire form
carries no field which reports the outcome.

Only images the request sends as pixels join its encoded-image budget as they
are added: at most 32 MiB across user and tool-result images. An image that
becomes a reference after its file is resolved adds no bytes. A request over
the budget fails during adaptation, before the remaining images are read.

Only the request changes: history, the Timeline and adopted input retain their
original image blocks. There is no persisted reference or cross-request cache.

### File import and read

`attachment.import {sessionId,path,name?,image?}` imports an authorized absolute
local path through `sessionFiles.import`. `image` defaults to false. `name`
defaults to `image` for images and `attachment` for ordinary files, and follows
the Session files name rule.

With `image=true`, preparation validates and scales before import. The reply is
`{attachmentId,path,bytes,image:{type:"file",mediaType,fileId}}`; ordinary files
return `image:null`. `attachmentId` equals the SessionStore file ID. Invalid image
input fails before Session ownership is committed.

`attachment.read {sessionId,fileId,offset}` delegates to `sessionFiles.read` and
returns `{data,eof}`: at most 64 KiB encoded as base64, empty at EOF. Both methods
reject a `sessionId` that differs from the provider's Session.

### A.2 `attachment.put`

Request: `{sessionId,name?,text}`. Text is nonempty UTF-8, at most 10 MiB;
`name` defaults to `paste.txt`. Reply: `{attachmentId,path,bytes}`.
SessionStore chooses the destination and commits through its `put` operation.

### A.3 CLI contract

The CLI prepares file and clipboard images, imports accepted bytes by a local
temporary path, and submits references after every import succeeds. Import errors
are reported before submission. The temporary source is released after the import call settles. Terminal markers
continue to show the accepted image's format and dimensions.

### A.4 Requirements to other owners

ModelProvider preserves each image's position in user and tool content and applies
A.1's routing after resolving file-backed sources. Read failures fail execution
and preserve history. Model changes route the stored images again; references are
computed only for images routed away from the current model.

## Appendix B. Full conflict table

| Conflict | Outcome |
| --- | --- |
| Session closing or owner reload before commit | Cancel pending import using owner settlement |
| Cancellation after commit | Keep committed file and result |
| Source changes while importing | Source length changes fail before publication |
| Fork followed by deleting source Session | Inherited file ownership keeps references readable |
| Foreign file ID | Refuse before returning file bytes or adopting input |

## Appendix C. Errors and recovery

| Owner/input error | RPC result |
| --- | --- |
| Wrong parameters, Session mismatch, invalid image source | `invalid_params` |
| Invalid name or empty text | `attachment_invalid` |
| Text over 10 MiB or imported file over 16 MiB | `attachment_too_large` |
| `not_found` | `session_closed` |
| `node_unavailable` | `node_unavailable` |
| `cancelled` | `service_reloading` |
| `storage` | `storage_failed` |

Unavailable methods use the Session method contract's `unknown_method`. Retries
are explicit and create new file identities.

## Appendix D. Acceptance

Exercise local path import and authenticated HTTP upload, bounded reads and
foreign-Session rejection; input and tool images through vision/text/vision
switching and provider encoding; restoration, inherited ownership and deletion;
source change, cancellation and failed import cleanup. Shared native and wire
operations must return equivalent owner results, including a 16 MiB import.

## Appendix G. Rust binding

`AttachmentsPlugin` declares `SessionIdentity`, `SessionFiles` and
`SessionMethods`, and registers its namespace during `apply`. Its binding delegates
to SessionStore's [Rust contract](../appserver/session-files.md#appendix-a-rust-binding).
