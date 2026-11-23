# Attachments

Scope: session. Issue: #2026.

Attachments owns the `attachment` methods: a client stores a large paste or imports a host file as a Session file and reads one back. The current implementation is in `crates/dim-agent-attachments/src/lib.rs`; the plugin page is [Attachments](../../architecture/rust-agent/plugins/Attachments.md). The Session file store is #2018's; this owner holds no state of its own.

## Use and scope: responsibility split

Attachments owns:

- **The three methods.** `attachment.put`, `attachment.import` and `attachment.read`, their input rules and their reply shapes.
- **The content rules.** Empty text, the 10 MiB paste limit, the 16 MiB image limit and the image re-encoding that keeps a stored image acceptable.
- **The ownership check.** The Session id in the request must be the calling Session's.

Callers keep:

- What a file is for: the model reads the returned path with `read`.
- The file's bytes and its name: the client supplies them; SessionStore owns the name rule and the storage cap.
- Attachment references: the image block's `fileId` is the Session file's id.

| Requirement (caller) | Compose from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A client stores a large paste as a Session file | No: the content rules and the store call are this owner's | `attachment.put` |
| A client imports a host file as a Session file | No: the image check, the re-encoding and the store call are this owner's | `attachment.import` |
| A client reads a stored attachment by range | No: the range read and the base64 reply are this owner's | `attachment.read` |
| The file is owned by this Session | No: the ownership check is this owner's | the same methods |
| The store's name rule and cap | Yes: SessionStore answers `invalid`/`too_large` | Business |

An operation or parameter exists only where the row says No.

## Caller example

A client stores a paste and reads it back:

```rust
let PutReply { attachment_id, path, bytes } = attachments.put(PutAttachment {
    session_id: session.clone(),
    name: Some("paste.txt".into()),
    text,
}, cancelled.clone()).await?;
let ReadReply { data, eof } = attachments.read(ReadAttachment {
    session_id: session,
    file_id: attachment_id,
    offset: 0,
}, cancelled).await?;
```

## Own contract

### Dependencies

| Service | Purpose |
| --- | --- |
| `S:sessionScope` | `identity` for the calling Session's id, used by the ownership check. |
| `P:sessionFiles` | The Session file store's `put`, `import` and `read` operations (#2018). |
| `S:sessionMethods` | Register the `attachment` namespace until this part unloads. |

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `attachment.put` | Store a paste as a Session file | `{sessionId, name?, text}` | `{attachmentId, path, bytes}` | The caller's token; a committed file stays | `invalid`, `too_large`, `not_found`, `storage`, `closed` |
| `attachment.import` | Copy a host file into the Session's files | `{sessionId, path, name?}` | `{attachmentId, path, bytes}` | The caller's token; a committed file stays | `invalid`, `too_large`, `not_found`, `storage`, `closed` |
| `attachment.read` | Read a stored attachment by offset | `{sessionId, fileId, offset}` | `{data, eof}` | The caller's token | `invalid`, `not_found`, `storage`, `closed` |

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid` | The text is empty, the Session id is not the calling Session's, or the parameters are malformed |
| `too_large` | The paste is over 10 MiB or the image over 16 MiB; store a smaller one |
| `not_found` | The Session file or the host path is gone |
| `storage` | The store failed; retry or report it |
| `closed` | The Session is closing; write nothing |

The Session method entry maps these kinds to the client codes `invalid_params`, `attachment_invalid`, `attachment_too_large`, `session_closed`, `storage_failed`; a cancelled call answers `service_reloading`, and a WebEntry transport failure keeps `node_unavailable`.

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `put`, `import` | Check the calling Session, check the content, call the store, answer the path | Before the store's commit nothing is stored; after it the file stays | The call ends; the store's commit settles | New calls are refused; in-flight calls settle | SessionStore for the file |
| `read` | Check the ownership, read the range, base64 | The read stops; nothing changes | The call ends | Answers `closed` once the Session is gone | SessionStore for the read |

## Exceptions with reasons

- The image is decoded and re-encoded before storage: a stored image must already be acceptable to the model, so the owner scales it once at the boundary instead of at every read.
- The reply's `path` is the model's handle: the model reads the file with `read`, and the file's ownership is the Session's.
- A foreign file id is refused before any bytes are returned: the calling Session's ownership is the whole authorization for a read, and an id or a path alone grants none.
- The per-request image view and its `img:` references belong to ModelProvider, not this owner: Attachments stores the bytes and returns the file id, and the request's view is computed where the request is built.

## Security

Source-path authorization belongs to the entry receiving the request: a browser cannot supply a host source path through RPC, and the importer accepts regular files within the Session file size limit. Image preparation opens the source, checks its length and applies the shared image format, pixel and scaling rules before SessionStore commits ownership; on Unix the open is nonblocking and rejects a terminal symlink. SessionStore chooses the destination and the permissions. A read requires the calling Session's ownership of the file id. The three methods are user actions under [ADR 003](../../adr/003-approval-scope.md), not agent-selected tool calls.

## CLI protocol

The CLI prepares file and clipboard images, imports the accepted bytes by a local temporary path, and submits the references only after every import succeeds. Import errors are reported before submission, and the temporary source is released once the import call settles. Terminal markers continue to show the accepted image's format and dimensions.

## Verification

- `put` stores a paste and answers its path and size; empty text and over-10-MiB text are refused; a name absent defaults to `paste.txt`.
- `import` copies a host file; a non-image over the limit and an unsupported image are refused; an image is re-encoded when needed.
- `read` pages by offset and answers `eof`; the Session id must match the calling Session.
- A store failure answers `storage`; a closing Session answers `closed`; nothing is stored on a refusal.

## Rule check

| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | One Session part; no state, no Timeline fact. |
| §2 scopes | Session: the methods serve one Session's files. |
| §3 orthogonality | No other owner's state is read; the store answers its own errors. |
| §4 rules 1–5, 9 | No command event, handoff, before callback, batch or Timeline fact. |
| §4 rule 6 | The stored path reaches the model as the method's reply. |
| §4 rule 7 | Clients use the three methods; no caller reads the store's tables. |
| §4 rule 8 | No infrastructure change. |
| §7 values and ids | The Session id, file id, path and bytes are data. |
| §7 no proxy | The store answers the file; this owner answers the method. |
| §7 one interface | One name, `attachment`, on the method table. |
| §7 cross-language | The methods are data. |
| §7 one change | The implementation task changes the method handlers in one change. |
| ADR 002 D1 | Session scope. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | The part depends on the Session file store. |
| ADR 002 D4 | No setting. |
| ADR 002 D5 | The contract is data. |

## Requirements raised to mechanism owners

1. [#2018](https://github.com/arcships/dim/issues/2018): the `sessionFiles` operations `put`, `import` and `read` with ids, and the image boundary this owner uses.

## Binding appendix

The `attachment` namespace is registered with the method table; the handlers are callbacks.

```rust
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutAttachment {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub text: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutReply { pub attachment_id: String, pub path: String, pub bytes: u64 }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadAttachment { pub session_id: String, pub file_id: String, pub offset: u64 }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadReply { pub data: String, pub eof: bool }

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentError {
    Invalid { message: String },
    TooLarge { message: String },
    NotFound,
    Storage { message: String },
    Closed,
}
```
