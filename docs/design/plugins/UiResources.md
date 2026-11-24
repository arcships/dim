# UiResources

Scope: Session. Issue: #2021.

UiResources owns the finished files a Session's parts publish for interfaces to read: their references, their reads and their withdrawal. This document is the owner's target contract; the current implementation is in `crates/dim-app-server/src/ui_extensions/resources.rs`. FileStorage, UiExtensions and the publishing parts are separate owners.

## Use and scope: responsibility split

UiResources owns:

- **Published references.** One per published file: the Session, the reference id, the open file, the media type and the publishing row instance.
- **Reads.** Bounded pages over one published file, counted so a withdrawal waits for them.
- **Ownership.** A reference belongs to the row that published it and ends when that row unloads or the Session closes; another row cannot withdraw it.
- **The Session's end.** Every reference and every read ends when the Session closes, without waiting for the part's unload.

Callers keep:

- Which file to publish: the publishing part chose a finalized regular file inside an authorized FileStorage root.
- What the interface shows: the client decides.
- The HTTP bytes: WebEntry's media route is a transport over the same read.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A Session part publishes a file for interfaces to read | No: the reference, its ownership and its reads are the owner's | `publish` |
| The publishing part withdraws the reference | No: withdrawal must wait for admitted reads | `withdraw` |
| An interface reads a page | No: the open file and the read counter are the owner's | `read` |

An operation or parameter exists only where the row says No.

## Caller example

A Session part publishes a render it produced:

```rust
let reference = resources.publish(PublishRequest {
    path: "/home/u/.dimcode/renders/chart.png".into(),
    media_type: "image/png".into(),
}).await?; // the reference names this Session; this row owns it
```

An interface reads it in pages:

```rust
let page = resources.read(ReadRequest { session_id, id: reference.id.clone(), offset: 0, limit: None }).await?;
```

## Own contract

### Dependencies

FileStorage's `open_read` (`{path} → {read_id, size}`), `read_at {read_id, offset}` and `close_read {read_id}`; SessionLifecycle's `closing`. The reader id stays inside UiResources: no client and no other owner sees it.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `publish` | Publish a finalized regular file for interfaces | `{path, media_type}` | `{session_id, id}` | While the file is opened; nothing is registered | `invalid_params`, `resource_unavailable`, `session_closed` |
| `withdraw` | Withdraw a reference this row published | `{reference}` | `{}` | None; the owner waits for admitted reads | `resource_gone`, `invalid_params`, `session_closed` |
| `read` | One bounded page of a published file | `{sessionId, id, offset?, limit?}` (the client method's flat shape) | `{offset, bytes, total, media_type, data}` | The caller's token; the page stops, the reference stays | `resource_gone`, `invalid_params`, `session_closed` |

`publish` takes the calling row's context as the binding's caller parameter, not as a request field: the reference belongs to the row that calls, and the owner registers the cleanup on that row. The FileStorage reader opened at `publish` is owned by UiResources and closed with the reference; its id never crosses. The Rust binding and the wire call the same method; rutis supplies the calling row's context on the wire.

The media type decides inline display: a listed image, audio, video, text or PDF type is served as declared; any other type is served as `application/octet-stream` and marked as an attachment.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `invalid_params` | The path is relative or outside every authorized root, the reference names another Session, or a page offset is beyond the file |
| `resource_unavailable` | The file is gone or unreadable; publish again or show the caller that the resource is unavailable |
| `resource_gone` | The reference was withdrawn; stop reading |
| `session_closed` | The Session is closing or is gone; the reference is settled |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `publish` | Check the Session phase, `open_read` the path in FileStorage, register the reference (with its read id and size) and the row's cleanup | The reader is closed, nothing is registered | The reference and its reader are removed by the row's cleanup | Every reference and reader is closed and removed | UiResources: the reference and the reader |
| `withdraw` | Check the owner row, cancel and wait for admitted reads, remove the reference | — | Already removed | Already removed | UiResources |
| `read` | Resolve the reference, take a read token, call FileStorage's `read_at` for one page, release the token | The page stops; the token is released | The read ends with the row | Admitted reads are cancelled before the reference and its reader go | UiResources: the token and the reader; FileStorage: the page |

The `session.ui.resources.read` client method is the same `read`, served to RPC clients; the web entry's HTTP route is its transport. Both stay.

## Exceptions with reasons

- `publish` takes `caller: &Ctx`: ownership and cleanup must bind the calling row, and only rutis knows which row is calling; a caller-supplied id would let one row publish a reference owned by another. The parameter is not a request field and no caller sends a row.
- WebEntry's media route calls `read` directly: the browser needs an HTTP response with range handling, and the route is a transport over the same read, not a second business path.

## Supersedes

- [Client UI extensions](../client-ui-extensions.md) service table row `uiResources`: the operations are those in this contract.
- [Service faces](../service-faces.md) §3.6's `uiResources` note.

## Rule check

The checks below cite [design rules §7, PR #2029](https://github.com/arcships/dim/pull/2029) because the section is not yet on `main`.


| Rule | Application |
| --- | --- |
| §1 parts and Capabilities | UiResources is one Session part; it writes no Timeline fact and is not a Capability. |
| §2 scopes | Session: references belong to one Session and end with it. |
| §3 orthogonality | No other owner's state is read; FileStorage and SessionLifecycle are asked. |
| §4 rules 1–6, 9 | No event, handoff, before callback, business meaning, batch, model context or Timeline fact. |
| §4 rule 7 | Callers use these operations; no caller sees an open file. |
| §7 no handle | The reference is a value with an id; the open file stays in the owner. |
| ADR 002 D1 | Session scope: a published reference is Session data. |
| ADR 002 D2 | No wiring plugin. |
| ADR 002 D3 | FileStorage is a process service this Session part may depend on. |
| ADR 002 D4 | No business setting; the root and the file are the caller's choice. |
| ADR 002 D5 | The contract is data; the Rust binding is the appendix. |

## Requirements raised to mechanism owners

None. FileStorage's contract (#2016) supplies `open_read`, `read_at` and `close_read`.

## Binding appendix

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishRequest { pub path: String, pub media_type: String }

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceRef { pub session_id: String, pub id: String }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadRequest {
    pub session_id: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcePage {
    pub offset: u64,
    pub bytes: usize,
    pub total: u64,
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ResourceError {
    #[error("UI resource is gone")]
    ResourceGone,
    #[error("{message}")]
    InvalidParams { message: String },
    #[error("{message}")]
    ResourceUnavailable { message: String },
    #[error("The Session is closing")]
    SessionClosed,
}

#[rutis::service(name = "uiResources")]
impl UiResources {
    pub async fn publish(&self, caller: &rutis::Ctx, request: PublishRequest) -> Result<ResourceRef, ResourceError> { /* ... */ }
    pub async fn withdraw(&self, caller: &rutis::Ctx, request: ResourceRef) -> Result<(), ResourceError> { /* ... */ }
    pub async fn read(&self, request: ReadRequest) -> Result<ResourcePage, ResourceError> { /* ... */ }
}
```

`caller: &Ctx` is rutis's caller parameter, filled by the wire dispatch from the calling row; it is not a request field.
