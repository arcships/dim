# Attachments

Owner: [dim-agent-attachments](../../../../crates/dim-agent-attachments/src/lib.rs).
Registers the `attachment` namespace with `sessionMethods`: `attachment.put` stores a large paste
as a Session file through SessionStore's `sessionFiles` and returns its path.
The model reads that path with `read` like any file. Design:
[Attachments](../../../design/plugins/Attachments.md).

## Dependencies

| Service | Purpose |
| --- | --- |
| `S:SessionIdentity` | The Session a put stores for; Rpc already resolved it from `sessionId`. |
| `P:SessionFilesHost` | SessionStore's `sessionFiles` data face (§9 of its owner contract). In this graph it is a graph-only fixture that stores nothing. |
| `S:SessionMethods` | Register the `attachment` namespace from `apply`, until this part unloads. |

## Method

`attachment.put {sessionId, name?, text}` (mutating, `drain`, no progress)
replies `{attachmentId, path, bytes}`, SessionStore's reply passed through with
`attachmentId = fileId`. `name` defaults to `paste.txt`.

```text
params ──> wrong shape? ───────────────> invalid_params (-32602)
   │       text empty? ────────────────> attachment_invalid
   │       text over 10 MiB? ──────────> attachment_too_large
   v
sessionFiles.put(own Session, name, UTF-8 bytes, call token)
   ├─ stored ─────────────────────────> {attachmentId, path, bytes}
   ├─ not_found ──────────────────────> session_closed
   ├─ invalid ────────────────────────> attachment_invalid
   ├─ too_large ──────────────────────> attachment_too_large
   ├─ node_unavailable ───────────────> node_unavailable
   ├─ cancelled ──────────────────────> service_reloading
   └─ storage ────────────────────────> storage_failed
```

Every code except `invalid_params` is `-32000`. SessionStore owns the name
rule and its 16 MiB cap; Attachments checks only its content rules.

## Events and lifetime

No Timeline events and no state. `drain`: an unload of the provider or of the
Rpc load that admitted a call fires the call's token, which SessionStore checks
right before its commit; the reply is then the path if the file committed,
otherwise `service_reloading` with nothing stored. A client disconnect cancels
nothing. Attachments reloads with SessionStore, its only process dependency,
which Agents already depend on.

## Installation

`serve` installs it next to Mcp's Session part when the host has native
storage and local execution (`native_home`); elsewhere `attachment.put` is
`unknown_method`. This graph installs it the same way.

## Tests

[attachments_flow.rs](../../../../crates/dim-app-server/tests/attachments_flow.rs)
covers `attachment.put` through Rpc (the reply, the model reading the file
with `read` without approval, rejections, a storage failure, a closed Session
and `unknown_method`), and an unload, a Session record gone before the commit
and an inactive node at the `sessionMethods` contract.

<!-- generated:plugin Attachments — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->
| 项 | 值 |
| --- | --- |
| 作用域 | Session |
| 注入 | `P:SessionFilesHost`、`S:SessionIdentity`、`S:SessionMethods` |
| 提供 | — |
| 被注入于 | — |
<!-- /generated -->

## File import and image reads

`attachment.import {sessionId, path, name?, image?}` is mutating and drains on
interruption. It calls `sessionFiles.import` for the owning Session. Names default
to `image` when `image=true`, otherwise `attachment`. The reply is
`{attachmentId, path, bytes, image}`; `image` is null for ordinary files, or
`{type:"file",mediaType,fileId}` for a validated image. Image preparation applies
the shared scaling rule to a pinned source before importing accepted bytes. Invalid
images fail before any Session ownership is committed. Import errors use the existing attachment error mapping.

`attachment.read {sessionId,fileId,offset}` is read-only and report-interrupted.
It delegates to `sessionFiles.read`, returning `{data,eof}`. Both methods verify
that the request's Session is the owning Session. Rpc admits path import only
from the local transport. The Web entry invokes it internally after receiving
an authenticated HTTP upload.
