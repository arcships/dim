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

export interface ApprovalCallData {
  name: string
  input: Record<string, unknown>
}

export interface ApprovalRequestData {
  approval_id: string
  source: BranchSource
  call: ApprovalCallData
  reason: string
}

export type ApprovalDecision = "allow" | "deny"

export interface Approval {
  /**
   * The committed requests no answer or cancellation ended, in request
   * order: what the `approval` face's `pending` reads.
   */
  pending(): Promise<ApprovalRequestData[]>
  /**
   * Answers request `request_id` with `decision`: what the `approval`
   * face's `decide` calls, and answered once the branch dispatched the
   * answer.
   * @throws {ApprovalRespondErrorThrown}
   */
  decide(requestId: string, decision: ApprovalDecision): Promise<void>
}

/** The connections attached to one Session now. */
export interface SessionPresenceRequest {
  sessionId: string
}

/**
 * The channels the model may see. `tui` is a UI leaf; a leaf's underlying
 * connection is not counted again.
 */
export type ClientChannel = "tui" | "cli" | "web" | "acp" | "remote" | "desktop" | "unknown"

/** One attached connection. */
export interface ClientPresence {
  channel: ClientChannel
  /** The client's self-declared `hello.clientName`; never an identity. */
  label?: string | null
}

/** The `presences` reply; an unknown Session is empty. */
export interface SessionPresencesReply {
  presences: ClientPresence[]
}

/**
 * The `sessionClientsHost` face: the connections attached to each Session,
 * as data. The App Server binds its connection and UI registries; a Rust
 * reader calls [`Self::presences`] directly and a part of another language
 * calls the same operation by name.
 */
export interface SessionClientsHost {
  /** The connections attached to `request.session_id` now. */
  presences(request: SessionPresenceRequest): Promise<SessionPresencesReply>
}

/**
 * One worktree association to record: the Session, the Git common directory
 * and worktree root it belongs to, and the Session's verified subtree. The
 * paths are UTF-8 strings, as every path that crosses is ([service faces
 * §1](../../../docs/design/service-faces.md#1-规则)).
 */
export interface WorktreeAssociation {
  sessionId: string
  repository: string
  root: string
  subtree: string
}

/** What `worktreeAssociations.associate` fails with: `{code, message}`. */
export type WorktreeAssociationError = { code: "failed"; message: string }

/**
 * The `worktreeAssociations` face: the association store records a verified
 * directory and its Session subtree atomically. The owner binds its write; a
 * Rust reader calls [`Self::associate`] directly and a part of another
 * language calls the same operation by name.
 */
export interface WorktreeAssociations {
  /**
   * Records `request`'s directory and Session subtree as one association.
   * @throws {WorktreeAssociationErrorThrown}
   */
  associate(request: WorktreeAssociation): Promise<void>
}

/** The `variables` reply: the names only, never a credential value. */
export interface CredentialVariablesReply {
  variables: string[]
}

/**
 * The `credentialVariablesHost` face: the names of the environment variables
 * the owner reads credentials from. The owner binds its read; a Rust reader
 * calls [`Self::variables`] directly and a part of another language calls the
 * same operation by name.
 */
export interface CredentialVariablesHost {
  /** The names, in the owner's set order. */
  variables(): Promise<CredentialVariablesReply>
}

export interface ToolExecutionContext {
  turnId: string
  requestId: string
  branchId: string
}

export interface ToolHost {
  /**
   * Runs one call through the host's tools as `caller` names the branch's
   * execution. The driver is the implementation; this is the `toolHost`
   * face ([tool contributions §2.1](../../../docs/architecture/rust-agent/tool-contributions.md#21-宿主工具的-toolhost-数据面)).
   * @throws {HostErrorThrown}
   */
  execute(call: { id: string; name: string; input: { [key: string]: unknown } }, caller: ToolExecutionContext, cancelled: () => void | Promise<void>): Promise<import('./plugin.js').ToolResult>
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

/** The identity of one committed input. */
export interface InputRef {
  inputId: string
  sequence: number | bigint
}

/**
 * A command to withdraw an input InputQueue still holds unapplied. It is
 * never a fact about the input by itself: only `input/deleted` is.
 */
export interface InputWithdrawData {
  input_id: string
  /** Text for clients to show with the deleted input; no decision reads it. */
  reason?: string | null
  /** Checked replacement content, adopted only if withdrawal is accepted. */
  steer?: InputMessageData | null
}

/** InputQueue's answer to one `input/withdraw`. */
export type Withdrawal =
  | /** The input was held: `input/deleted` at this sequence, never applied. */ { status: "deleted"; sequence: number | bigint }
  | /** Unknown, already applied or already deleted: nothing was written. */ { status: "notHeld" }

/**
 * Consumes `input/message` and decides by its mode when Loop may consume
 * it, writing `input/applied` with the mode then: steer and append inputs
 * at once; a queue input when no turn runs or is about to start,
 * otherwise after the turn ends, one per turn. A handover input is never
 * released: it waits for the turn it names and is deleted when that turn
 * ends, or on arrival when no such turn is open. Knows of turns only
 * through `turn/start`, `turn/end` and `stop/interrupt`; Loop decides how
 * to consume what is applied. Consumes `input/withdraw` in order and
 * writes `input/deleted` for an input it still holds unapplied.
 */
export interface InputQueue {
  /**
   * Commits an input through the branch entry, accepting each submission id once.
   * @throws {AgentErrorThrown}
   */
  submit(input: InputMessageData): Promise<InputRef>
  /**
   * Withdraws a held input without replacing it. Steer candidates must enter
   * through `session.input.withdraw`, which validates and checks them first.
   * @throws {AgentErrorThrown}
   */
  withdraw(command: InputWithdrawData): Promise<Withdrawal>
}

/**
 * One declared method as the reservation question reads it: the name, whether
 * it mutates and the notification names it may send. `interruption` and
 * `progress` belong to an actual registration, not to the question, so they do
 * not cross.
 */
export interface ReservedMethod {
  name: string
  mutating: boolean
  notifications?: string[]
}

/** A namespace and the declarations it would register. */
export interface ReservedMethodsRequest {
  namespace: string
  declarations?: ReservedMethod[]
}

/** What `reservedMethods` fails with: `{code, message}`. */
export type ReservedMethodsError = { code: "reserved"; message: string }

/**
 * The method names a host keeps from Session namespaces, such as its
 * protocol's built-ins and roots: a process value the Session's table
 * consults when a namespace is registered. A host that serves no protocol
 * names no such value, and its Session tables reserve nothing.
 */
export interface ReservedMethods {
  /**
   * Refuses a namespace and declarations the host keeps for itself.
   * @throws {ReservedMethodsErrorThrown}
   */
  check(request: ReservedMethodsRequest): Promise<void>
}

export type Decision =
  | { decision: "allow" }
  | { decision: "ask"; reason: string }
  | { decision: "deny"; reason: string }

export interface Permission {
  /**
   * Decides `call` now, as the `permission` face's `check` does: the
   * dispatched policy, the file facts and the command rules of the moment,
   * with no advisor (Hooks A.6.1 asks advisors for admission only).
   * @throws {HostErrorThrown}
   */
  check(call: ApprovalCallData, risk: string | null, cancelled: () => void | Promise<void>): Promise<Decision>
}

/**
 * The operator's reply-language choices, by their stable wire codes. The
 * names are the ones the operator selects, named in English for the prompt
 * text ([PromptSources §3]).
 *
 * [PromptSources §3]: ../../../docs/design/plugins/PromptSources.md
 */
export type ReplyLanguage = "en" | "zh-CN" | "zh-Hant" | "ja" | "ko" | "id" | "de" | "fr" | "es" | "pt-BR" | "pt-PT"

/**
 * The `get` reply: the Session's reply language when the operator selected
 * one, `null` otherwise.
 */
export interface SessionLanguageReply {
  language?: ReplyLanguage | null
}

/**
 * The Session's reply language, when the operator selected one: the live
 * value the `dim/language` source reads and the App Server's
 * `session.language.*` methods change. It starts from `SessionConfig.language`
 * when the Session is created or restored.
 */
export interface SessionLanguage {
  /**
   * The reply language the operator selected, as data. The App Server's
   * `session.language.get` answers the same value; its update also writes
   * the Session record and has Prompt adopt the source, so changing the
   * value is not this face's operation.
   */
  get(): Promise<SessionLanguageReply>
}

/** The `get` reply: the factory instructions text, when the assembly set one. */
export interface SkillsConfigReply {
  instructions?: string | null
}

/**
 * Skills' factory configuration: the text that replaces [`INSTRUCTIONS`]
 * ([model-prompts §9.2](../../../docs/design/model-prompts.md#92-every-capability-that-writes-model-facing-text)).
 */
export interface SkillsConfig {
  /**
   * The instructions text the assembly configured, as data; `null` means
   * the built-in [`INSTRUCTIONS`].
   */
  get(): Promise<SkillsConfigReply>
}

export interface TimelineInfo {
  id: string
  parent?: string | null
  branch_point?: number | bigint | null
  create_anchor?: number | bigint | null
  source?: BranchSource | null
}

/** Which branches a query covers. */
export type TimelineQueryScope =
  | /** The queried branch alone. */ "branch"
  | /** The branch chain: each ancestor up to its cut, root first, then the queried branch. */ "chain"

export type QueryOperand =
  | { kind: "field"; path: string }
  | { kind: "outer"; path: string }
  | { kind: "root"; path: string }
  | { kind: "concat"; parts: QueryOperand[] }
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

export type WaitResult =
  | { reason: "notification"; taskIds: string[]; waitedMs: number | bigint }
  | { reason: "user_steer"; waitedMs: number | bigint }
  | { reason: "timeout"; waitedMs: number | bigint }
  | { reason: "interrupted"; waitedMs: number | bigint }

export interface Wait {
  /**
   * Waits for the next matching arrival, the timeout or the caller's
   * cancellation, as the `wait` tool's call does.
   *
   * The caller cancels the call with `cancelled`; the wait then ends as
   * [`WaitResult::Interrupted`] and that answer stands. A caller that
   * drops the native Rust future releases the single-wait slot. A bridge
   * caller must signal `cancelled` before abandoning its reply.
   * @throws {AlreadyWaitingThrown}
   */
  wait(taskId: string | null, timeoutMs: number | bigint, cancelled: () => void | Promise<void>): Promise<WaitResult>
}

export type TaskError =
  | /** Required task facts could not be read. */ { read: [] }
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
  | /** Required task facts could not be read. */ { read: [] }
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
  | /** Required task facts could not be read. */ { read: [] }
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

export interface ModelId {
  providerId: string
  modelId: string
}

/** The `get` reply: the Agent's identity and default model. */
export interface AgentConfigReply {
  name: string
  systemPrompt: string
  model: ModelId
}

export interface AgentConfig {
  /**
   * The Agent's identity and default model as data, for parts in other
   * languages.
   */
  get(): Promise<AgentConfigReply>
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

/**
 * The `get` reply: each description text the assembly configured, or `null`
 * for the built-in one.
 */
export interface CollaborationConfigReply {
  leaderDescription?: string | null
  memberDescription?: string | null
}

/**
 * Collaboration's factory configuration: the two `agent` descriptions, each
 * a whole text in place of its built-in one (Collaboration §3).
 */
export interface CollaborationConfig {
  /** The two `agent` descriptions the assembly configured, as data. */
  get(): Promise<CollaborationConfigReply>
}

/** One member relation as `collaborationDirectory.relations` answers it. */
export interface CollaborationRelation {
  memberSessionId: string
  leaderSessionId: string
  memberAgentId: string
}

/** The `relations` reply: every member relation this process holds. */
export interface CollaborationRelationsReply {
  relations: CollaborationRelation[]
}

/** Process-lifetime records, independent of reloadable AgentIndex logic. */
export interface CollaborationDirectory {
  /** Every member relation this process holds, by member Session id. */
  relations(): Promise<CollaborationRelationsReply>
}

/**
 * Admission for process-owned work, including work accepted before a Session
 * exists. A hold starts before acceptance and ends after the last owned effect.
 */
export interface ProcessActivity {
  /**
   * Admits work unless admission is paused. Release the returned id after
   * settlement. A lost owner leaves admission busy until process shutdown;
   * an unload or crash is not evidence that its accepted work settled.
   */
  hold(): Promise<number | bigint | null>
  /** Releases a settled operation's hold. Already released holds return false. */
  release(id: number | bigint): Promise<boolean>
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

/**
 * The `read` reply: the addresses as UTF-8 text, as every path that crosses
 * is ([service faces §1](../../../docs/design/service-faces.md#1-规则)).
 */
export interface BoundAddresses {
  unixPath?: string | null
  pipeName?: string | null
  websocketAddress?: string | null
}

/** What `listenAddresses.read` fails with: `{code, message}`. */
export type ListenAddressesError = { code: "invalid_path"; message: string }

/** Addresses actually bound by the transport plugin. */
export interface ListenAddresses {
  /**
   * The addresses this process bound, if any.
   * @throws {ListenAddressesErrorThrown}
   */
  read(): Promise<BoundAddresses>
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

/** What a method returning `Err(AlreadyWaiting)` throws. */
export type AlreadyWaitingThrown = RustError<"AlreadyWaiting", "AlreadyWaiting", undefined>

/** What a method returning `Err(ApprovalRespondError)` throws. */
export type ApprovalRespondErrorThrown = RustError<"ApprovalRespondError", "Unknown" | "Cancelled" | "Closed", undefined>

/** What a method returning `Err(HostError)` throws. */
export type HostErrorThrown = RustError<"HostError", "Failed" | "Cancelled", undefined>

/** What a method returning `Err(ListenAddressesError)` throws. */
export type ListenAddressesErrorThrown = RustError<"ListenAddressesError", "invalid_path", ListenAddressesError>

/** What a method returning `Err(McpAppError)` throws. */
export type McpAppErrorThrown = RustError<"McpAppError", "Absent" | "Unsupported" | "NotCallable" | "UnknownServer" | "Unavailable" | "Refused" | "Unknown", undefined>

/** What a method returning `Err(McpError)` throws. */
export type McpErrorThrown = RustError<"McpError", "Settings" | "InvalidId" | "UnknownServer" | "InvalidEntry" | "MoveConflict" | "MovePartial" | "CredentialsAreUserScoped" | "StaleSecretRecord" | "ImportConflict" | "ImportPartial" | "Disabled" | "Unavailable" | "Server" | "Auth" | "Closed" | "Cancelled" | "Worker", undefined>

/** What a method returning `Err(OutputPageError)` throws. */
export type OutputPageErrorThrown = RustError<"OutputPageError", "read" | "unknown" | "unavailable" | "closed" | "invalidLimit", OutputPageError>

/** What a method returning `Err(ReservedMethodsError)` throws. */
export type ReservedMethodsErrorThrown = RustError<"ReservedMethodsError", "reserved", ReservedMethodsError>

/** What a method returning `Err(ResourceError)` throws. */
export type ResourceErrorThrown = RustError<"ResourceError", string, ResourceError>

/** What a method returning `Err(SessionsError)` throws. */
export type SessionsErrorThrown = RustError<"SessionsError", "rejected" | "agent_unknown" | "session_closed" | "branch_missing" | "branch_ended" | "reloading" | "future_sequence" | "ambiguous" | "publication_unknown" | "failed", SessionsError>

/** What a method returning `Err(TaskError)` throws. */
export type TaskErrorThrown = RustError<"TaskError", "read" | "unknown" | "closed", TaskError>

/** What a method returning `Err(WaitError)` throws. */
export type WaitErrorThrown = RustError<"WaitError", "read" | "unknown" | "closed" | "timeout", WaitError>

/** What a method returning `Err(WorkflowFilesError)` throws. */
export type WorkflowFilesErrorThrown = RustError<"WorkflowFilesError", "invalid_path" | "closed" | "cancelled" | "locked" | "unsupported" | "unsynced" | "io" | "not_found" | "invalid_data", WorkflowFilesError>

/** What a method returning `Err(WorktreeAssociationError)` throws. */
export type WorktreeAssociationErrorThrown = RustError<"WorktreeAssociationError", "failed", WorktreeAssociationError>

/** The exported services by name: `ctx.use<Services['name']>('name')`. */
export interface Services {
  approval: Approval
  sessionClientsHost: SessionClientsHost
  worktreeAssociations: WorktreeAssociations
  credentialVariablesHost: CredentialVariablesHost
  toolHost: ToolHost
  inputQueue: InputQueue
  reservedMethods: ReservedMethods
  permission: Permission
  sessionLanguage: SessionLanguage
  skillsConfig: SkillsConfig
  treeCommands: TreeCommands
  wait: Wait
  backgroundExec: BackgroundExec
  agentConfig: AgentConfig
  sessionConfig: SessionConfig
  timelineIdentity: TimelineIdentity
  sessionIdentity: SessionIdentity
  branchLifecycle: BranchLifecycle
  sessionLifecycle: SessionLifecycle
  dispatcher: Dispatcher
  timeline: Timeline
  partRegistry: PartRegistry
  sessionPermissions: SessionPermissions
  collaborationConfig: CollaborationConfig
  collaborationDirectory: CollaborationDirectory
  processActivity: ProcessActivity
  mcpSession: McpSession
  workflowFiles: WorkflowFiles
  listenAddresses: ListenAddresses
  uiResources: UiResources
}
