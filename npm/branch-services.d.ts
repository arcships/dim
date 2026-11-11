// Generated from the service declarations; do not edit.

/** What a method's `Err(e)` throws: named after `e`'s type, `code` its variant, `message` its `Display`, `data` its serde value. */
export interface RustError<Name extends string = string, Code extends string = string, Data = unknown> extends Error {
  readonly name: Name
  readonly code: Code
  readonly data: Data
}

export type BranchSource =
  | { kind: "tool"; turn_id: string; request_id: string; call_id: string }
  | { kind: "host"; operation_id: string }

export interface TimelineInfo {
  id: string
  parent?: string | null
  branch_point?: number | bigint | null
  create_anchor?: number | bigint | null
  source?: BranchSource | null
}

export type ImageSourceType = "base64" | "url"

export interface ImageSource {
  type: ImageSourceType
  mediaType: string
  data?: string | null
  url?: string | null
}

export type ContentBlock = { type: "text"; text: string } | { type: "image"; source: ImageSource }

export interface UserInput {
  content: ContentBlock[]
}

/**
 * Runtime content a capability places in the model context. The renderer
 * puts `text` verbatim as one user message where the input is consumed.
 */
export type InputBlock = { type: "raw"; text: string; key?: string | null; retire?: boolean }

export type InputMode = "queue" | "steer" | "append"

/**
 * The handover one input is addressed to: only the continuation of that
 * handover consumes it, and InputQueue deletes it otherwise.
 */
export interface InputHandover {
  turn_id: string
  request_id: string
}

export interface InputMessageData {
  submission_id?: string | null
  user?: UserInput | null
  blocks?: InputBlock[]
  mode?: InputMode | null
  handover?: InputHandover | null
}

/** Which branches a query covers. */
export type TimelineQueryScope =
  | /** The queried branch alone. */ "branch"
  | /** The branch chain: each ancestor up to its cut, root first, then the queried branch. */ "chain"

export type QueryOperand =
  | { kind: "field"; path: string }
  | { kind: "outer"; path: string }
  | { kind: "element"; path: string }
  | { kind: "value"; value: unknown }

export type QueryComparison = "eq" | "ne" | "lt" | "le" | "gt" | "ge"

export type QueryFilter =
  | { op: "compare"; left: QueryOperand; comparison: QueryComparison; right: QueryOperand }
  | { op: "in"; value: QueryOperand; values: unknown[] }
  | { op: "present"; value: QueryOperand }
  | { op: "prefix"; value: QueryOperand; prefix: string }
  | { op: "contains"; value: QueryOperand; text: string }
  | /** A superset of rows whose partition's concatenated property contains every term after Unicode lowercasing. Missing partition keys never join different events. Consumers must verify their own text; this condition cannot be negated. */ { op: "text_candidate"; field: string; partition_by: string[]; terms: string[] }
  | { op: "and"; conditions: QueryFilter[] }
  | { op: "or"; conditions: QueryFilter[] }
  | { op: "not"; condition: QueryFilter }
  | { op: "exists"; condition: QueryFilter }
  | { op: "any"; field: string; condition: QueryFilter }

export type QuerySelect =
  | { kind: "fields"; fields: string[] }
  | { kind: "events" }
  | { kind: "count" }
  | { kind: "characters"; field: string }
  | { kind: "exists" }

export type QueryPick = "first" | "last"

export type QueryOrderKey = { kind: "position" } | { kind: "field"; path: string } | { kind: "count" }

/** Direction of one result ordering key. */
export type TimelineQueryDirection =
  | /** Increasing key values; position order runs from root to branch. */ "ascending"
  | /** Decreasing key values. */ "descending"

export interface QueryOrder {
  key: QueryOrderKey
  direction: TimelineQueryDirection
}

export interface QuerySpec {
  filter?: QueryFilter | null
  select: QuerySelect
  group_by?: string[]
  pick?: QueryPick | null
  order_by?: QueryOrder[]
}

/** Missing properties and array projections retain their shape independently of JSON null. */
export type QueryValue =
  | { kind: "missing" }
  | { kind: "value"; value: unknown }
  | { kind: "elements"; values: QueryValue[] }

/** Continuation data bound to the query and logical scope, without server-side state. */
export interface QueryCursor {
  binding: string
  keys: QueryValue[]
}

export interface QueryPage {
  limit: number | bigint
  after?: QueryCursor | null
}

/** A property read at a fixed branch boundary (ADR 022). */
export interface TimelineQuery {
  scope: TimelineQueryScope
  from?: { branch_id: string; sequence: number | bigint } | null
  through?: number | bigint | null
  include_publishing?: boolean
  query: QuerySpec
  page?: QueryPage | null
}

export interface QueryRow {
  group: QueryValue[]
  values: QueryValue[]
}

export interface QueryEvent {
  group: QueryValue[]
  position: { branch_id: string; sequence: number | bigint }
  event: import('./plugin.js').BranchEvent
}

export type QueryOutput =
  | { kind: "rows"; rows: QueryRow[] }
  | { kind: "events"; events: QueryEvent[] }
  | { kind: "count"; count: number | bigint }
  | { kind: "exists"; exists: boolean }

export interface TimelineQueryReply {
  through: number | bigint
  result: QueryOutput
  next?: QueryCursor | null
}

export interface BranchOptions {
  prompt: string
  source: BranchSource
}

/** Shared with Timeline roots; never owns the tree or its child roots. */
export interface TreeCommands {
  list(): Promise<TimelineInfo[]>
  /**
   * The directory record of branch `id`, from its reservation on, so a
   * part that loads with its own line reads it during its replay.
   * @throws {AgentErrorThrown}
   */
  info(id: string): Promise<TimelineInfo>
  /**
   * Commits `input` to branch `id` through its validating `Branch::input`
   * and returns the committed event, or the input already committed with
   * its submission id.
   * @throws {AgentErrorThrown}
   */
  input(id: string, input: InputMessageData): Promise<import('./plugin.js').BranchEvent>
  /**
   * Ends the line `id` with `source`; only the target is checked (§10).
   * @throws {AgentErrorThrown}
   */
  end(id: string, source: BranchSource): Promise<void>
  /**
   * One page of `id`'s committed batches up to `through`; an ended line
   * reads at its fixed head and `through` must end an atomic batch.
   * @throws {AgentErrorThrown}
   */
  page(id: string, after: number | bigint, limit: number | bigint, through: number | bigint): Promise<import('./plugin.js').BranchEvent[]>
  /**
   * Computes properties of one line at a fixed cutoff. Chain scope is invalid.
   * An ended line reads its persisted facts and has no publishing tail.
   * @throws {AgentErrorThrown}
   */
  query(id: string, query: TimelineQuery): Promise<TimelineQueryReply>
  /**
   * Creates a child of `parent` cut at `at` and returns its id.
   * `cancelled` is the caller's cancellation as an explicit argument, the
   * form every cross-language contract uses for it (C4's `cancelled`): the
   * creation's admission is cancelled, as [`Self::branch`]'s token does,
   * once the future it returns completes, or fails because the caller is
   * gone. Dropping the call cancels it too. TimelineTree records the outcome.
   * Cancellation is observed through that future, so it ends only a
   * creation still open when the future completes: a child already built
   * returns its id, however early the caller's cancellation completed on
   * its own side.
   * @throws {AgentErrorThrown}
   */
  create(parent: string, at: number | bigint, options: BranchOptions, cancelled: () => void | Promise<void>): Promise<string>
}

export type TaskError =
  | /** No matching task or live output on this branch. */ "unknown"
  | /** This instance was unloaded. */ "closed"

/**
 * One page of a task's registered output file, as
 * `session.exec.output.page` answers it.
 */
export interface OutputPage {
  /** The page's text, sanitized like the tail preview. */
  text: string
  /**
   * The file bytes this page consumed; a client's next cursor is always
   * `cursor + bytes`. `text` can be shorter because sanitization drops
   * control characters.
   */
  bytes: number | bigint
  /**
   * Present when the read found at least `limit` bytes, so the client may
   * continue immediately; always `cursor + bytes`. Absent on a shorter or
   * empty page, which does not mean the file ended: the client advances by
   * `bytes` and asks again.
   */
  nextCursor?: number | bigint | null
  /** Whether the output file reached `OUTPUT_FILE_BYTE_LIMIT`. */
  truncated: boolean
}

/** Why one paged read of a task's registered output file answered nothing. */
export type OutputPageError =
  | /** No matching task on this branch. */ "unknown"
  | /** The registered output file is gone or unreadable: it was cleaned, or the task's Session was deleted. */ "unavailable"
  | /** This instance was unloaded. */ "closed"
  | /** The page budget was zero. */ "invalidLimit"

export type ExecStatus = "completed" | "failed" | "cancelled"

export interface ExecEndedData {
  task_id: string
  status: ExecStatus
  result: import('./plugin.js').ToolResult
}

/** Why waiting for a task's end answered nothing. */
export type WaitError =
  | /** No matching task on this branch. */ "unknown"
  | /** This instance was unloaded, or the task's record was released. */ "closed"
  | /** The wait's own timeout elapsed first. */ "timeout"

export interface BackgroundExec {
  /**
   * Reads the live capture without reconstructing a resource from history:
   * what `backgroundExec.output` answers.
   * @throws {TaskErrorThrown}
   */
  output(taskId: string): Promise<string>
  /**
   * Reads one page of the task's registered output file, by byte offset:
   * what `backgroundExec.outputPage` answers. `limit` rejects 0 and clamps
   * to [`OUTPUT_PAGE_BYTES_MAX`]; a dropped call drops the read.
   * @throws {OutputPageErrorThrown}
   */
  outputPage(taskId: string, cursor: number | bigint, limit: number | bigint): Promise<OutputPage>
  /**
   * Waits until the task ends, or `timeout_ms` elapses: what
   * `backgroundExec.wait` answers. An ended task answers its record at
   * once; a dropped call releases the wait.
   * @throws {WaitErrorThrown}
   */
  wait(taskId: string, timeoutMs: number | bigint): Promise<ExecEndedData>
  /**
   * Stops a task: cancels it while running, so it ends `cancelled` and
   * does not wake. Returns once its `exec/ended` is dispatched; a task that
   * already ended keeps its result and its wake.
   * @throws {TaskErrorThrown}
   */
  stop(taskId: string): Promise<ExecEndedData>
}

export interface SessionConfig {
  /**
   * The creation-time configuration as data, for parts in other
   * languages.
   */
  get(): Promise<import('./plugin.js').SessionConfig>
}

export interface TimelineIdentity {
  /** The identity as data, for parts in other languages. */
  get(): Promise<import('./plugin.js').SessionRef>
}

export interface SessionIdentity {
  /**
   * The identity as data, `{sessionId, restored}`, for parts in other
   * languages.
   */
  get(): Promise<{ sessionId: string; restored: boolean }>
}

/**
 * Lifecycle of one branch: provided by its scope, advanced only by the
 * branch's [`crate::TimelineContext`]. Capabilities that spawn background
 * work in `apply` wait for [`Self::activated`] before doing it.
 */
export interface BranchLifecycle {
  /**
   * Waits until the branch leaves `Loading`: true when it became Active,
   * false when it closes without ever starting.
   */
  activated(): Promise<boolean>
  /**
   * Waits until the branch starts closing; background work ends then,
   * without waiting for its capability to unload.
   */
  closing(): Promise<void>
}

/**
 * Lifecycle of one Session, provided by its scope. Session capabilities that
 * spawn background work in `apply`, such as replacing Session-owned instances
 * or contacting external services, wait for [`Self::activated`] and end that
 * work when [`Self::closing`] returns. A Session whose assembly fails never
 * activates, and readiness of its assembly never depends on that work.
 *
 * Its Agent publishes it (`Active`) and the runtime records `Closed` after
 * close or failed assembly. Closing follows the Session's close admission:
 * the phase is `Closing` from the moment its closing token is cancelled, by
 * `Session::close`, by `Agent::close` or during assembly, so no observer sees
 * a closing Session as active.
 *
 * Owners of accepted detached work take a [`SessionHold`] with
 * [`Self::take_hold`], which keeps an idle release from ending that work;
 * parts in other languages use [`Self::hold`].
 */
export interface SessionLifecycle {
  /**
   * Waits until the Session is published or starts closing: true only when
   * it was published and is not closing.
   */
  activated(): Promise<boolean>
  /**
   * Waits until the Session starts closing; background work ends then,
   * without waiting for its capability to unload.
   */
  closing(): Promise<void>
  /**
   * An activity hold for parts in other languages, as [`Self::take_hold`]
   * takes one: the id of a hold the calling row owns. It lasts until
   * [`Self::release`] releases it or the calling row unloads, which
   * releases every hold the row still has. No hold (`None`, `null` in
   * TypeScript) once the Session is closing or its idle release is
   * decided.
   */
  hold(): Promise<number | bigint | null>
  /**
   * Releases the hold `id` took and ends its cleanup in the row that
   * took it: true when it was held, false when it was released already
   * or by its row's unload.
   * @throws {AgentErrorThrown}
   */
  release(id: number | bigint): Promise<boolean>
}

/** Delivers one branch's committed events on that branch's private channel. */
export interface Dispatcher {
  /**
   * Registers an ordered consumer that rebuilds: the subscribe and rebuild
   * steps of the capability reload protocol as one registration (plugin
   * contract §5.2). It subscribes like [`Self::subscribe`] and holds the
   * facts that match the keys, calls `rebuild` once, which reads the facts
   * the part folds through this branch's `timeline` and returns the cutoff
   * that read took, then delivers the held facts and every later matching
   * fact above that cutoff, in sequence order and none twice. Live facts
   * wait until `rebuild` returns, so work it leaves to do, such as terminals
   * for open operations, is done before them. A `rebuild` failure is
   * returned and lets no live fact through; live failures and the lifetime
   * are [`Self::subscribe`]'s.
   * @throws {AgentErrorThrown}
   */
  consume(name: string, keys: string[], rebuild: () => number | bigint | Promise<number | bigint>, handler: (arg0: import('./plugin.js').BranchEvent) => void | Promise<void>): Promise<void>
  /**
   * [`Self::subscribe`] in data form, for parts in other languages: the
   * calling row's `ctx`, owned keys, and a handler taking the committed
   * event by value whose failure is the subscriber's handler failure.
   * @throws {AgentErrorThrown}
   */
  listen(name: string, keys: string[], handler: (arg0: import('./plugin.js').BranchEvent) => void | Promise<void>): Promise<void>
  /**
   * Last sequence assigned on this branch, published or not: the end a
   * rebuild's read reaches, and the position live delivery continues from.
   */
  committed(): Promise<number | bigint>
  notify(): Promise<void>
  sequence(): Promise<number | bigint>
  changed(): Promise<void>
}

export interface Timeline {
  branchId(): Promise<string>
  ancestors(): Promise<[string, number | bigint][]>
  /**
   * Appends one event; it is published once its batch commits. Parts in
   * other languages write through it like native capabilities (ADR 001).
   * @throws {AgentErrorThrown}
   */
  append(original: { type: string; name: string; data: unknown; tags?: string[]; context_render?: boolean; context_state?: boolean }): Promise<import('./plugin.js').BranchEvent>
  /**
   * Published sequence: what is durable and visible to the Dispatcher
   * and to readers outside the branch.
   */
  sequence(): Promise<number | bigint>
  /**
   * Last sequence assigned on this bus, published or not. An event is a
   * fact once it has its sequence; the reads below cover it.
   */
  committed(): Promise<number | bigint>
  /** Pending events participate in idle/close accounting before publication. */
  hasPending(): Promise<boolean>
  /**
   * Computes properties over one fixed branch or ancestry scope.
   * The optional start must name a position inside that scope.
   * @throws {AgentErrorThrown}
   */
  query(query: TimelineQuery): Promise<TimelineQueryReply>
}

/** The scope a part is registered for. */
export type PartScope = "session" | "branch"

/**
 * The parts plugins register for Sessions and branches. One registry serves
 * every Agent built from an [`AgentBuilder`](crate::AgentBuilder); the host
 * hands it to its process plugins ([`AgentBuilder::parts`](crate::AgentBuilder::parts)).
 * Plugins of other languages reach it by the process name `partRegistry`.
 */
export interface PartRegistry {
  /**
   * Registers a plugin definition for `scope`: every open instance of the
   * scope mounts one copy of it, and so does every later one, each in the
   * runtime that defined it with its `inject` resolved in that instance.
   * `name` is the part's name, as for a generator.
   *
   * The definition belongs to the row that handed it over. When that row
   * unloads, rutis ends every copy first; the registration then goes.
   */
  registerDefinition(scope: PartScope, name: string, definition: import('@arcships/rutis').Plugin | ((ctx: any, config: any) => unknown) | { apply(ctx: any, config: any): unknown }): Promise<void>
}

/** A branch of a Session. */
export interface SessionRef {
  sessionId: string
  branchId: string
}

/**
 * The dispatched policy; `version` is the local sequence of the last
 * `permission/changed` of this branch, 0 when there is none.
 */
export interface PermissionState {
  version: number | bigint
  policy: import('./plugin.js').PermissionPolicy
}

/** A Session service failure; generated exports throw this serde value. */
export type SessionsError =
  | { code: "rejected"; message: string }
  | { code: "agent_unknown"; message: string }
  | { code: "session_closed"; message: string }
  | { code: "branch_missing"; message: string }
  | { code: "branch_ended"; message: string }
  | { code: "reloading"; message: string }
  | { code: "future_sequence"; message: string }
  | { code: "ambiguous"; message: string }
  | { code: "publication_unknown"; message: string }
  | { code: "failed"; message: string }

export interface Empty {
}

/** Read-only policy access through the stable Agents directory. */
export interface SessionPermissions {
  /**
   * Reads the branch's dispatched policy while holding its Session lease.
   * @throws {SessionsErrorThrown}
   */
  permission(request: SessionRef): Promise<PermissionState>
  /**
   * Reads the current host bound without creating or choosing a Session.
   * @throws {SessionsErrorThrown}
   */
  permissionDefaults(request: Empty): Promise<import('./plugin.js').PermissionPolicy>
}

export type McpSourceState =
  | { state: "disabled" }
  | { state: "connecting" }
  | { state: "ready"; version: string }
  | { state: "failed"; message: string }
  | /** The transport is gone but the last tools stay published and callable. The reason is kept until a connection succeeds. */ { state: "disconnected"; message: string }
  | /** The server needs the user to sign in; clears by itself once the server's credential changes. */ { state: "authRequired"; expired: boolean }

export interface McpSessionState {
  servers: Record<string, McpSourceState>
  catalogError?: string | null
  closed: boolean
}

export type SchemaType = "object" | "array" | "string" | "number" | "integer" | "boolean" | "null"

/**
 * A tool input schema.
 *
 * Deserialization accepts the JSON Schema forms an MCP server may send and
 * stores the canonical shape the v22 format defines: a `type` array of one
 * name becomes that name, two or more names become `anyOf` branches (under
 * `allOf` when the node already has an `anyOf`), and a boolean subschema
 * becomes `{}` or `{"not": {}}`. Keywords the struct does not type, such as
 * `anyOf` branches or `$defs`, stay verbatim in `extensions` and are never
 * walked. A rejected node's error names its path under `inputSchema`.
 */
export type JsonSchema = {
  type?: SchemaType | null
  properties?: Record<string, JsonSchema> | null
  items?: JsonSchema | null
  required?: string[] | null
  enum?: unknown[] | null
  description?: string | null
} & Record<string, unknown>

/**
 * The mount read's reply (MCP Apps §4.1). `text` and `blob` are exclusive;
 * `app` is the document's own `_meta.ui`, which the host applies.
 */
export interface McpWidgetResult {
  server: string
  name: string
  description?: string | null
  inputSchema: JsonSchema
  uri: string
  mimeType: string
  text?: string | null
  blob?: string | null
  app?: Record<string, unknown> | null
}

export type ToolContent = string | ContentBlock[]

/**
 * A view's tool result (MCP Apps §4.2), `_meta` included. It is the reply to
 * the client that asked; the Timeline's `ToolResult` is never built from it.
 */
export interface McpAppCallResult {
  content: ToolContent
  structuredContent?: Record<string, unknown> | null
  isError?: boolean | null
  meta?: Record<string, unknown> | null
}

/** Management view, without credentials, transport handles or registry access. */
export interface McpSession {
  /**
   * This Session's entries and their states: what the `mcpSession`
   * face's `state` reads.
   */
  state(): Promise<McpSessionState>
  /**
   * Requests rediscovery, including external file edits and failed connections.
   * Completion is reflected in state and B's ToolSourcesView, not this return.
   * @throws {McpErrorThrown}
   */
  refresh(): Promise<void>
  /**
   * The widget a published tool declares (MCP Apps D4): its entry, the
   * server's own tool name, the document that entry's connection serves,
   * and the tool definition a host applies to the view. What the
   * `mcpSession` face's `widget` answers; a dropped call cancels the
   * document read.
   * @throws {McpAppErrorThrown}
   */
  widget(published: string): Promise<McpWidgetResult>
  /**
   * One tool call on one entry of this Session, on that entry's own
   * connection (MCP Apps D5). It is the user's action on an enabled entry,
   * not an agent decision: no branch admits it and it writes no fact. What
   * the `mcpSession` face's `call` answers; a dropped call cancels the
   * server request.
   * @throws {McpAppErrorThrown}
   */
  call(server: string, tool: string, arguments: Record<string, unknown>): Promise<McpAppCallResult>
}

export type WorkflowRoot = { project: string } | { package: string }

export interface WorkflowReadRequest {
  root: WorkflowRoot
  path: string
}

export type WorkflowFilesError =
  | { code: "invalid_path"; message: string }
  | { code: "closed"; message: string }
  | { code: "cancelled"; message: string }
  | { code: "locked"; message: string }
  | { code: "unsupported"; message: string }
  | { code: "unsynced"; message: string }
  | { code: "io"; message: string }
  | { code: "not_found"; message: string }
  | { code: "invalid_data"; message: string }

export interface WorkflowDirectoryRequest {
  root: WorkflowRoot
}

/** What a directory entry is, without following links. */
export type EntryKind =
  | "file"
  | "directory"
  | /** A symbolic link, reparse point or special file. Storage operations never follow it. */ "other"

export interface WorkflowFileEntry {
  name: string
  kind: EntryKind
}

export interface WorkflowReplaceRequest {
  root: WorkflowRoot
  path: string
  bytes: Uint8Array
}

/** Generated access to authorized project definitions and the calling row's packages. */
export interface WorkflowFiles {
  /**
   * Reads raw bytes from one .yaml basename; no S10 envelope or text conversion.
   * @throws {WorkflowFilesErrorThrown}
   */
  read(request: WorkflowReadRequest): Promise<Uint8Array>
  /**
   * Lists the fixed definition directory; non-UTF-8 names fail without aliases.
   * @throws {WorkflowFilesErrorThrown}
   */
  list(request: WorkflowDirectoryRequest): Promise<WorkflowFileEntry[]>
  /**
   * Atomically replaces a project definition. Package selectors return invalid_path.
   * @throws {WorkflowFilesErrorThrown}
   */
  replace(request: WorkflowReplaceRequest): Promise<void>
  /**
   * Ensures only the project's .dimcode and .dimcode/workflows directories.
   * @throws {WorkflowFilesErrorThrown}
   */
  createDirectory(request: WorkflowDirectoryRequest): Promise<void>
}

export interface PublishFile {
  /** Absolute host path inside an already authorized FileStorage root. */
  path: string
  mediaType: string
}

export interface ResourceRef {
  sessionId: string
  id: string
}

export interface ResourceError {
  code: string
  message: string
}

export interface UiResources {
  /**
   * Publishes a finalized regular file at a canonical absolute host path
   * inside an already authorized FileStorage root; relative paths are invalid.
   * The calling part owns the reference until withdrawal or unload.
   * @throws {ResourceErrorThrown}
   */
  publishFile(request: PublishFile): Promise<ResourceRef>
  /**
   * Withdraws only a reference this calling part published; waits for reads to end.
   * @throws {ResourceErrorThrown}
   */
  withdraw(reference: ResourceRef): Promise<void>
}

/** What a method returning `Err(AgentError)` throws. */
export type AgentErrorThrown = RustError<"AgentError", "InvalidInput" | "Closed" | "UnknownBranch" | "BranchEnded" | "Storage" | "PublicationUnknown" | "Build" | "Cancelled" | "Reloading" | "AmbiguousSubmission" | "PartialSubmission" | "Fault" | "Framework" | "Multiple", undefined>

/** What a method returning `Err(McpAppError)` throws. */
export type McpAppErrorThrown = RustError<"McpAppError", "Absent" | "Unsupported" | "NotCallable" | "UnknownServer" | "Unavailable" | "Refused" | "Unknown", undefined>

/** What a method returning `Err(McpError)` throws. */
export type McpErrorThrown = RustError<"McpError", "Settings" | "InvalidId" | "UnknownServer" | "InvalidEntry" | "MoveConflict" | "MovePartial" | "CredentialsAreUserScoped" | "StaleSecretRecord" | "ImportConflict" | "ImportPartial" | "Disabled" | "Unavailable" | "Server" | "Auth" | "Closed" | "Cancelled" | "Worker", undefined>

/** What a method returning `Err(OutputPageError)` throws. */
export type OutputPageErrorThrown = RustError<"OutputPageError", "unknown" | "unavailable" | "closed" | "invalidLimit", OutputPageError>

/** What a method returning `Err(ResourceError)` throws. */
export type ResourceErrorThrown = RustError<"ResourceError", string, ResourceError>

/** What a method returning `Err(SessionsError)` throws. */
export type SessionsErrorThrown = RustError<"SessionsError", "rejected" | "agent_unknown" | "session_closed" | "branch_missing" | "branch_ended" | "reloading" | "future_sequence" | "ambiguous" | "publication_unknown" | "failed", SessionsError>

/** What a method returning `Err(TaskError)` throws. */
export type TaskErrorThrown = RustError<"TaskError", "unknown" | "closed", TaskError>

/** What a method returning `Err(WaitError)` throws. */
export type WaitErrorThrown = RustError<"WaitError", "unknown" | "closed" | "timeout", WaitError>

/** What a method returning `Err(WorkflowFilesError)` throws. */
export type WorkflowFilesErrorThrown = RustError<"WorkflowFilesError", "invalid_path" | "closed" | "cancelled" | "locked" | "unsupported" | "unsynced" | "io" | "not_found" | "invalid_data", WorkflowFilesError>

/** The exported services by name: `ctx.use<Services['name']>('name')`. */
export interface Services {
  treeCommands: TreeCommands
  backgroundExec: BackgroundExec
  sessionConfig: SessionConfig
  timelineIdentity: TimelineIdentity
  sessionIdentity: SessionIdentity
  branchLifecycle: BranchLifecycle
  sessionLifecycle: SessionLifecycle
  dispatcher: Dispatcher
  timeline: Timeline
  partRegistry: PartRegistry
  sessionPermissions: SessionPermissions
  mcpSession: McpSession
  workflowFiles: WorkflowFiles
  uiResources: UiResources
}
