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

export interface InputMessageData {
  submission_id?: string | null
  user?: UserInput | null
  blocks?: InputBlock[]
  mode?: InputMode | null
}

/** Which branches a query covers. */
export type TimelineQueryScope =
  | /** The queried branch alone. */ "branch"
  | /** The branch chain: each ancestor up to its cut, root first, then the queried branch. */ "chain"

/**
 * The key a read orders by; a chain read orders by chain position first, so
 * `sequence` is the sequence within an event's own branch.
 */
export type TimelineQueryOrderBy = "sequence"

/**
 * The end a page is taken from. A reply is always ordered by chain position
 * and then sequence, in this direction; the offset counts from that end.
 */
export type TimelineQueryDirection =
  | /** Oldest first: the page is the oldest matching facts, and `offset` starts it further along. */ "ascending"
  | /** Newest first: the page is the newest matching facts, and `offset` starts it further back. */ "descending"

/**
 * One of the envelope's two context flags a query may name. Naming either
 * returns the events that carry at least one named flag.
 */
export type TimelineQueryFlag =
  | /** The flag that puts an event in the rendered context. */ "render"
  | /** The flag that puts an event in the projection's checkpoint state. */ "state"

/** How one predicate compares the JSON value at its path. */
export type PredicateOp =
  | /** The value equals `value`. */ "eq"
  | /** The value is one of the scalars in `value`. */ "in"
  | /** The value is a string starting with `value`. */ "prefix"
  | /** The value is a string containing `value`. */ "contains"
  | /** The path exists; `value` is null. */ "exists"

/** One comparison against one JSON scalar of an event's data. */
export interface TimelineQueryPredicate {
  /**
   * Field names inside the event's data joined by dots; no array indexing
   * and no wildcards.
   */
  path: string
  /** The comparison; absent reads as `Eq`. */
  op?: PredicateOp
  /** The scalar `Eq` compares, or the scalars `In` accepts. */
  value: unknown
}

/** Whether every term must appear or one is enough. */
export type TextMatch = "all" | "any"

/**
 * A text selection over the event data. The store selects candidates; the
 * caller verifies what the rendered content says.
 */
export interface TextSearch {
  terms: string[]
  mode?: TextMatch
}

/**
 * One bounded read of a branch's facts: what it covers, how far it reaches,
 * what it accepts, and how much of the result it returns.
 */
export interface TimelineQuery {
  scope: TimelineQueryScope
  /**
   * The event the read begins at, named by its id; the event is included.
   * At most one of this and `from_sequence` may be given.
   */
  from_event?: string | null
  /**
   * The sequence the read begins at; the event at it is included. At most
   * one of this and `from_event` may be given.
   */
  from_sequence?: number | bigint | null
  /** The key the reply is ordered by, with chain position first. */
  order_by?: TimelineQueryOrderBy
  /** The end a page is taken from; ascending is the default. */
  direction?: TimelineQueryDirection
  /**
   * Whether the queried branch's own facts written above the published head
   * belong to the read, up to the point the reply reports as `through`. An
   * ancestor is read to its cut either way.
   */
  include_publishing?: boolean
  /** Event kinds (`domain/name`) the read accepts; empty accepts every kind. */
  kinds?: string[]
  /**
   * The context flags the read accepts; empty constrains nothing, and a
   * non-empty set accepts events carrying at least one named flag.
   */
  flags?: TimelineQueryFlag[]
  /** When named, only the event with this id. */
  event_id?: string | null
  /** One predicate the event's data must satisfy. */
  predicate?: TimelineQueryPredicate | null
  /** Further predicates the event's data must satisfy; all are ANDed. */
  predicates?: TimelineQueryPredicate[]
  /** A text selection over the event data; absent selects without one. */
  text?: TextSearch | null
  /** How many matching events the page returns; nonzero by type. */
  limit: number | bigint
  /** How many matching events the read skips before the page. */
  offset?: number | bigint
}

/**
 * The facts a query read, in chain order and then sequence, in the query's
 * direction, each with its position in the same order.
 */
export interface TimelineQueryReply {
  events: import('./plugin.js').BranchEvent[]
  /**
   * One position per event, in the same order: the branch the event was
   * read from and its sequence there.
   */
  positions: ({ branch_id: string; sequence: number | bigint })[]
  /**
   * The sequence the read is cut at on the queried branch: what was
   * published when it ran, or the committed head it asked for. No fact
   * above it was read; an ancestor is read to its cut. A caller holding
   * live events releases the ones above this sequence.
   */
  through: number | bigint
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
   * Queries one line's own facts, forwarding to its Timeline while open
   * and reading at the tree's fixed head after it ends. Chain scope is
   * invalid; an ended line has no publishing tail.
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
   * One bounded read over this branch or over its chain ([ADR 019](../../../../docs/adr/019-event-query.md),
   * [plugin contract §5.4](../../../../docs/design/plugin-contract.md#54-the-event-query)):
   * the direction, kinds, flags, event id, predicate, limit and offset
   * select the facts, and the reply is up to `limit` of them in chain order
   * and then sequence, in the query's direction, each with its position. A
   * chain read is each ancestor to its cut and then this branch, root first
   * ascending and this branch first descending; the offset the earlier
   * segment did not use moves to the next one, so paging is skipping and a
   * page shorter than the limit means the selection is exhausted. The read
   * never returns a fact above a branch's coverage: an ancestor to its cut,
   * this branch to the published head, or to the committed head when the
   * query asks for it, in which case this branch's own facts in
   * `published+1..=through` are part of the same page.
   *
   * The reply's `through` is the cutoff the read took on this branch: the
   * published head, or the committed head the query asked for, which is
   * read when the call starts, so a write during the call stays out of the
   * page. The published part and the tail are disjoint in sequence, so each
   * fact is returned once. A caller that holds live events releases the ones
   * above the cutoff ([ADR 020 D3](../../../../docs/adr/020-timeline-read-convergence.md)).
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
export type AgentErrorThrown = RustError<"AgentError", "InvalidInput" | "Closed" | "UnknownBranch" | "BranchEnded" | "Storage" | "Build" | "Cancelled" | "Reloading" | "AmbiguousSubmission" | "Fault" | "Framework" | "Multiple", undefined>

/** What a method returning `Err(ResourceError)` throws. */
export type ResourceErrorThrown = RustError<"ResourceError", string, ResourceError>

/** What a method returning `Err(SessionsError)` throws. */
export type SessionsErrorThrown = RustError<"SessionsError", "rejected" | "agent_unknown" | "session_closed" | "branch_missing" | "branch_ended" | "reloading" | "future_sequence" | "ambiguous" | "publication_unknown" | "failed", SessionsError>

/** What a method returning `Err(WorkflowFilesError)` throws. */
export type WorkflowFilesErrorThrown = RustError<"WorkflowFilesError", "invalid_path" | "closed" | "cancelled" | "locked" | "unsupported" | "unsynced" | "io" | "not_found" | "invalid_data", WorkflowFilesError>

/** The exported services by name: `ctx.use<Services['name']>('name')`. */
export interface Services {
  treeCommands: TreeCommands
  sessionConfig: SessionConfig
  timelineIdentity: TimelineIdentity
  sessionIdentity: SessionIdentity
  branchLifecycle: BranchLifecycle
  sessionLifecycle: SessionLifecycle
  dispatcher: Dispatcher
  timeline: Timeline
  partRegistry: PartRegistry
  sessionPermissions: SessionPermissions
  workflowFiles: WorkflowFiles
  uiResources: UiResources
}
