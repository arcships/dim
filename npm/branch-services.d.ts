// Generated from the service declarations; do not edit.

/** What a method's `Err(e)` throws: named after `e`'s type, `code` its variant, `message` its `Display`, `data` its serde value. */
export interface RustError<Name extends string = string, Code extends string = string, Data = unknown> extends Error {
  readonly name: Name
  readonly code: Code
  readonly data: Data
}

/**
 * The document form of the `execution` settings source, and the switches the
 * host reads at process start. It is also the `read` reply of
 * `executionSettingsHost`: the two switches are the whole document.
 */
export interface ExecutionSettings {
  sandbox: boolean
  network: boolean
}

/** What `executionSettingsHost` fails with: `{code, message}`. */
export type ExecutionSettingsError = { code: "failed"; message: string }

/**
 * The `executionSettingsHost` face: its owner provides it with the switches'
 * read, another language calls `read` by name, and a Rust reader calls the
 * same method directly.
 */
export interface ExecutionSettingsHost {
  /**
   * The switches in effect now.
   * @throws {ExecutionSettingsErrorThrown}
   */
  read(): Promise<ExecutionSettings>
}

export interface ModelId {
  providerId: string
  modelId: string
}

/**
 * One non-conversational call of a model: the catalogued model a process
 * adapter routes work to, and the `<category>.<operation>` it serves.
 */
export interface ModalityTarget {
  target: ModelId
  operation: string
}

export type ModelProtocol =
  | "openai-chat"
  | "openai-compatible"
  | "openai-responses"
  | "xai"
  | "xai-responses"
  | "anthropic"
  | "gemini"
  | "bedrock"

/**
 * Request-local transport facts. Providers validate these at their input
 * boundary; the model host owns clients, not persisted provider records.
 */
export interface ModelNetwork {
  proxy?: string | null
  timeoutMs?: number | bigint | null
}

export interface HttpModelConnection {
  protocol: ModelProtocol
  baseUrl: string
  network: ModelNetwork
  headers: Record<string, string>
  bearer?: string | null
  apiKey?: string | null
  preset?: string | null
}

export interface AwsCredentials {
  accessKeyId: string
  secretAccessKey: string
  sessionToken?: string | null
}

export type BedrockAuth = { bearer: string } | { sigV4: { credentials: AwsCredentials; region: string } }

export interface BedrockModelConnection {
  baseUrl: string
  network: ModelNetwork
  headers: Record<string, string>
  auth: BedrockAuth
}

/**
 * Connection facts are resolved for one final request and never serialized.
 * Local is the explicit development host profile, with no network credentials.
 */
export type ModelConnection = "local" | { http: HttpModelConnection } | { bedrock: BedrockModelConnection }

/**
 * The realtime transport a catalog model declares in its entry's
 * `metadata.raw.realtime`: the protocol the adapter speaks and the path under
 * the connection's base URL. The path is declared, never derived from the
 * base URL.
 */
export interface RealtimeDeclaration {
  protocol: string
  path: string
}

/**
 * A model's connection for one non-conversational call, as the
 * `resolveModalityConnection` reply: the connection facts the adapter sends
 * the call with, the route the catalog carries and the model id. The adapter
 * holds it for that call only; the owner never persists it.
 */
export interface ModalityConnection {
  /** The connection a chat call to the same model would use. */
  connection: ModelConnection
  /** The route the catalog carries for this model. */
  protocol: ModelProtocol
  /** The model id a request sends to the provider. */
  model: string
  /**
   * The realtime transport the model's catalog entry declares, for a
   * `realtime.*` operation. `None` on every other use, and on a model that
   * declares no transport.
   */
  realtime?: RealtimeDeclaration | null
}

/**
 * What a process model port fails with: `{code, message}`. The owner's
 * [`HostError`] becomes one of these codes where the face is served, so a
 * caller of any language reads the same vocabulary.
 */
export type ModelPortError =
  | /** The caller's request is not one the owner admits. */ { code: "invalid_request"; message: string }
  | /** The model named is not one the owner serves for this use. */ { code: "model_unavailable"; message: string }
  | /** The caller ended the call. */ { code: "cancelled"; message: string }
  | /** The owner is unloading; the call can succeed once it reloads. */ { code: "owner_closed"; message: string }
  | /** The owner's read failed. */ { code: "failed"; message: string }

/**
 * The `modalityConnectionHost` face: its owner provides it with the
 * resolution, another language calls `resolveModalityConnection` by name, and
 * a Rust adapter calls the same method directly.
 */
export interface ModalityConnectionHost {
  /**
   * The connection one non-conversational call of `target` uses.
   * @throws {ModelPortErrorThrown}
   */
  resolveModalityConnection(target: ModalityTarget): Promise<ModalityConnection>
}

/**
 * The `resolve` request of `modelSelectionHost`: the model the caller names,
 * if any, and the directory a project's configuration is read from.
 */
export interface ModelSelectionRequest {
  explicit?: ModelId | null
  cwd?: string | null
}

/**
 * The adopted capabilities of a model: an exact value a Session carries and
 * ContextCompaction reads. Both limits are required and positive; absence is
 * a property of the catalog, which carries [`CatalogCapabilities`] and
 * converts through [`CatalogCapabilities::declared`].
 */
export interface ModelCapabilities {
  modalities: string[]
  toolCalling: boolean
  streaming: boolean
  structuredOutput: boolean
  vision: boolean
  audio: boolean
  reasoning: boolean
  contextWindow: number | bigint
  maxOutputTokens: number | bigint
  providerState?: Record<string, unknown> | null
}

export interface ReasoningBudget {
  min?: number | null
  max?: number | null
  default?: number | null
  unit?: string | null
}

export type ReasoningOff =
  | { kind: "thinking_type"; value: string }
  | { kind: "thinking_budget"; value: number | bigint }
  | { kind: "enable_thinking"; value: boolean }
  | { kind: "chat_template_kwargs"; keys: string[] }

export interface ModelReasoning {
  supported: boolean
  defaultEnabled?: boolean | null
  mode?: string | null
  effort?: string | null
  effortOptions?: string[] | null
  level?: string | null
  levelOptions?: string[] | null
  verbosity?: string | null
  verbosityOptions?: string[] | null
  interleaved?: boolean | null
  summaries?: boolean | null
  visibility?: string | null
  continuation?: string[] | null
  budget?: ReasoningBudget | null
  notes?: string[] | null
  off?: ReasoningOff | null
}

export interface ModelServiceTier {
  supported: boolean
  default?: string | null
  options?: string[]
}

export interface ModelCapabilitySnapshot {
  target: ModelId
  capabilities: ModelCapabilities
  reasoning?: ModelReasoning | null
  serviceTier?: ModelServiceTier | null
}

export type StartupAuth = "anonymous" | "apiKey" | "bearer"

/**
 * Non-secret connection declaration. Credentials are rebound in Provider
 * memory; this identity remains valid across Session unload and restore.
 */
export interface StartupConnectionSource {
  id: string
  apiFormat: ModelProtocol
  baseUrl: string
  auth: StartupAuth
  headerNames: string[]
}

/**
 * Adopted startup policy, copied with the model at a branch cut and into
 * newly created native members and summary requests. Contains no secrets.
 */
export interface ModelStartup {
  connection?: StartupConnectionSource | null
  autoCompactWindow?: number | bigint | null
}

export type RequestMetadata = {
  sessionId?: string | null
  runId?: string | null
  traceId?: string | null
  turnId?: string | null
  startup?: ModelStartup | null
  /**
   * The Agent profile whose prompt the branch runs: the profile it last
   * adopted. An adoption from a profile records it; a model selection and a
   * preference update carry it into the new configuration, and only
   * another adoption replaces it.
   */
  agentId?: string | null
} & Record<string, unknown>

/**
 * The model-owned part of configuration. Tool declarations and system content
 * are adopted by their own capabilities, not supplied by a provider.
 */
export interface ModelSelection {
  providerId: string
  modelId: string
  capabilitySnapshot?: ModelCapabilitySnapshot | null
  config?: Record<string, unknown> | null
  metadata?: RequestMetadata | null
}

/**
 * The `modelSelectionHost` face: its owner provides it with the resolution,
 * another language calls `resolve` by name, and a Rust reader calls the same
 * method directly.
 */
export interface ModelSelectionHost {
  /**
   * The model one request resolves to.
   * @throws {ModelPortErrorThrown}
   */
  resolve(request: ModelSelectionRequest): Promise<ModelSelection>
}

/**
 * The `selectableModels` reply: the models the owner would serve, with the
 * rules execution applies.
 */
export interface SelectableModelsReply {
  models: ModelCapabilitySnapshot[]
}

/**
 * One model Provider would select for a non-conversational use, with the
 * `<category>.<operation>` ids its catalog entry declares (Modality §9.2).
 * It carries no limits: such a use does not run a chat request.
 */
export interface ModalityModelSnapshot {
  target: ModelId
  /** The declared operation ids, in the catalog's order. */
  operations: string[]
}

/**
 * The `selectableForModality` reply: the models the owner would serve for a
 * non-conversational use.
 */
export interface ModalityModelsReply {
  models: ModalityModelSnapshot[]
}

/**
 * The `selectableModelsHost` face: its owner provides it with both
 * enumerations, another language calls them by name, and a Rust reader calls
 * the same methods directly.
 */
export interface SelectableModelsHost {
  /**
   * The models the owner would serve, as execution would select them.
   * @throws {ModelPortErrorThrown}
   */
  selectableModels(): Promise<SelectableModelsReply>
  /**
   * The models the owner would select for a non-conversational use, with
   * the modality operation ids each declares: the same provider, account,
   * model, route, credential and status rules as [`Self::selectable_models`],
   * without its text-modality, chat-kind and declared-text-limit
   * requirements. A model that declares no modality operation is omitted.
   * @throws {ModelPortErrorThrown}
   */
  selectableForModality(): Promise<ModalityModelsReply>
}

/**
 * The `prompt-suggestion` document. The Settings boundary applies the
 * defaults and the range before the part reads it, so both members are
 * present in every document either of them reads; a missing one is an error,
 * never an invented value.
 */
export interface PromptSuggestionSettings {
  enabled: boolean
  maxChars: number
}

/** What `promptSuggestionSettingsHost` fails with: `{code, message}`. */
export type PromptSuggestionSettingsError = { code: "failed"; message: string }

/**
 * The `promptSuggestionSettingsHost` face: its owner provides it with the
 * document's read, another language calls `read` by name, and the Session
 * part calls the same method directly.
 */
export interface PromptSuggestionSettingsHost {
  /**
   * The `prompt-suggestion` document as its source holds it now.
   * @throws {PromptSuggestionSettingsErrorThrown}
   */
  read(): Promise<PromptSuggestionSettings>
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

/**
 * One installed runtime in the `list` reply. The two directories are UTF-8
 * paths, as every path that crosses is
 * ([service faces §1](../../../docs/design/service-faces.md#1-规则)).
 */
export interface ToolchainView {
  bin: string
  root: string
}

/**
 * The `list` reply of `toolchainSourceHost`: the installed runtimes, in the
 * fixed order node, python, then uv.
 */
export interface ToolchainSourceReply {
  toolchains: ToolchainView[]
}

/** What `toolchainSourceHost` fails with: `{code, message}`. */
export type ToolchainSourceError = { code: "failed"; message: string }

/**
 * The `toolchainSourceHost` face: its owner provides it with the runtimes,
 * another language calls `list` by name, and a Rust reader calls the same
 * method directly.
 */
export interface ToolchainSourceHost {
  /**
   * The installed runtimes, in order.
   * @throws {ToolchainSourceErrorThrown}
   */
  list(): Promise<ToolchainSourceReply>
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
export type AgentErrorThrown = RustError<"AgentError", "InvalidInput" | "Closed" | "UnknownBranch" | "BranchEnded" | "Storage" | "Build" | "Cancelled" | "Reloading" | "AmbiguousSubmission" | "PartialSubmission" | "Fault" | "Framework" | "Multiple", undefined>

/** What a method returning `Err(ExecutionSettingsError)` throws. */
export type ExecutionSettingsErrorThrown = RustError<"ExecutionSettingsError", "failed", ExecutionSettingsError>

/** What a method returning `Err(ModelPortError)` throws. */
export type ModelPortErrorThrown = RustError<"ModelPortError", "invalid_request" | "model_unavailable" | "cancelled" | "owner_closed" | "failed", ModelPortError>

/** What a method returning `Err(PromptSuggestionSettingsError)` throws. */
export type PromptSuggestionSettingsErrorThrown = RustError<"PromptSuggestionSettingsError", "failed", PromptSuggestionSettingsError>

/** What a method returning `Err(ResourceError)` throws. */
export type ResourceErrorThrown = RustError<"ResourceError", string, ResourceError>

/** What a method returning `Err(SessionsError)` throws. */
export type SessionsErrorThrown = RustError<"SessionsError", "rejected" | "agent_unknown" | "session_closed" | "branch_missing" | "branch_ended" | "reloading" | "future_sequence" | "ambiguous" | "publication_unknown" | "failed", SessionsError>

/** What a method returning `Err(ToolchainSourceError)` throws. */
export type ToolchainSourceErrorThrown = RustError<"ToolchainSourceError", "failed", ToolchainSourceError>

/** What a method returning `Err(WorkflowFilesError)` throws. */
export type WorkflowFilesErrorThrown = RustError<"WorkflowFilesError", "invalid_path" | "closed" | "cancelled" | "locked" | "unsupported" | "unsynced" | "io" | "not_found" | "invalid_data", WorkflowFilesError>

/** The exported services by name: `ctx.use<Services['name']>('name')`. */
export interface Services {
  executionSettingsHost: ExecutionSettingsHost
  modalityConnectionHost: ModalityConnectionHost
  modelSelectionHost: ModelSelectionHost
  selectableModelsHost: SelectableModelsHost
  promptSuggestionSettingsHost: PromptSuggestionSettingsHost
  treeCommands: TreeCommands
  toolchainSourceHost: ToolchainSourceHost
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
