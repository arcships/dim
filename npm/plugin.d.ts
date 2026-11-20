import type { AgentEvent, JsonValue, JsonObject } from "./client-protocol.js";
export type { AgentEvent, JsonValue, JsonObject } from "./client-protocol.js";
/** Named services implemented by Dim's App Server. No runtime code is exported. */
export type FileError = 'closed' | 'exists' | 'invalid_data' | 'invalid_path' | 'internal' | 'io' | 'locked';
export type FileResult<T> = { ok: true; value: T } | { ok: false; error: FileError };

export type FileKind = 'file' | 'directory' | 'other';
/** One entry of `FileScope.list`: a path relative to the authorized root, ready
 * for another operation, and what it is without following links. */
export interface FileEntry { name: string; kind: FileKind }

export interface FileLease {
  read(): Promise<FileResult<string | null>>;
  replace(text: string): Promise<FileResult<null>>;
  backup(suffix: string): Promise<FileResult<null>>;
  /** The raw file, envelope included, up to 8 MiB; null when absent. Text
   * files are read as their stored envelope bytes. */
  readBytes(): Promise<FileResult<Uint8Array | null>>;
  /** Replaces the file with raw bytes, up to 8 MiB, atomically. No envelope. */
  replaceBytes(bytes: Uint8Array): Promise<FileResult<null>>;
}

export interface FileScope {
  writeStream(name: string, next: () => Uint8Array | null | Promise<Uint8Array | null>): Promise<FileResult<null>>;
  directory(name: string): Promise<FileResult<string>>;
  openRead(name: string): Promise<FileResult<Awaited<ReturnType<FsHost["openRead"]>> | null>>;
  withLock<T>(path: string, work: (file: FileLease) => T | Promise<T>, cancelled?: Promise<void>): Promise<FileResult<T>>;
  /** One level of the directory `prefix`, sorted, never entering a directory
   * entry and never listing the root itself. */
  list(prefix: string): Promise<FileResult<FileEntry[]>>;
  /** Removes a file, or a directory and everything beneath it; `locked` while
   * a lease holds it or a file beneath it. */
  remove(name: string): Promise<FileResult<null>>;
  quiesce(): null;
  close(): Promise<null>;
}

export interface PluginFiles {
  open(): Promise<FileScope>;
}

/** JSON data carried by the native bridge. Integers outside JavaScript's safe range
 * arrive as bigint; sending back bigint requires a signed or unsigned 64-bit value.
 * Numbers must be finite. This is not the external client's JSON encoding. */
export type BridgeJsonValue = null | boolean | number | bigint | string | BridgeJsonValue[] | BridgeJsonObject;
export type BridgeJsonObject = { [key: string]: BridgeJsonValue };

/** Project roots are process-local numbers, not durable identities. */
export type SettingsScope = 'user' | { project: number };
export interface SettingsSnapshot { revision: string; document: JsonObject }
export type SettingsErrorCode = 'unknown_source' | 'invalid_scope' | 'invalid_document' |
  'user_only_field' | 'conflict' | 'unsynced' | 'failed' | 'closed' | 'cancelled';
export type SettingsResult<T> = { ok: true; value: T } |
  { ok: false; error: { code: SettingsErrorCode; message: string } };

/** A JSON Schema source: `schema` is draft 2020-12 with an object root; `userOnly` lists
 * JSON Pointers to members only the user changes. */
export interface SourceDeclaration {
  source: string; userFile: string; projectFile?: string; schema: JsonObject; userOnly?: string[];
}

/** Process service `settings`. */
export interface Settings {
  declaredBy(request: { instanceId: string }): SourceDeclaration[];
  /** Registers a source for as long as the calling row is loaded; its documents stay in the
   * home when the row unloads. Rejects (failing the caller's apply) when the declaration is
   * invalid or its name or a file is registered already. Call it from a row's own `apply`. */
  register(declaration: SourceDeclaration): Promise<void>;
  read(request: { source: string; scope: SettingsScope }): Promise<SettingsResult<SettingsSnapshot>>;
  update(request: { source: string; scope: SettingsScope; expectedRevision: string; document: JsonObject }): Promise<SettingsResult<SettingsSnapshot>>;
  projects(request: Record<string, never>): SettingsResult<{ projects: { root: number; cwd: string }[] }>;
}

export type TitleSource = 'auto' | 'manual';
export interface Title { text: string; source: TitleSource }
/** `revision` counts committed title writes; 0 means never written. */
export interface SessionMetadataSnapshot { revision: number; title: Title | null }
export type SessionMetadataResult<T> = { ok: true; value: T } |
  { ok: false; error: { code: 'conflict' | 'not_found' | 'storage'; message: string } };
/** Process service `sessionMetadata`, provided by SessionStore. `setTitle` is handed over
 * when the host receives it: a lost reply neither cancels nor undoes it, so read again. */
export interface SessionMetadata {
  read(request: { sessionId: string }): Promise<SessionMetadataResult<SessionMetadataSnapshot>>;
  setTitle(request: { sessionId: string; expectedRevision: number; title: Title | null }):
    Promise<SessionMetadataResult<{ revision: number }>>;
}

export interface ModelId { providerId: string; modelId: string }
export interface AuxiliaryRequest {
  system?: string | null;
  /** Serialized dim-agent-types Message values, validated by Provider. */
  messages: JsonObject[];
  maxOutputTokens?: number | null;
}
export interface AuxiliaryReply {
  text: string;
  stopReason: 'end_turn' | 'max_tokens' | 'stop_sequence';
  usage?: {
    promptTokens: number; completionTokens: number; totalTokens: number;
    cacheReadTokens?: number; cacheWriteTokens?: number;
    /** The part of `completionTokens` spent thinking, already inside it and
     * in `totalTokens`. It is the count the protocol adapter gives: absent
     * when the adapter gives none, and zero when Gemini or a Responses adapter
     * reads a missing field as zero, as it does for the cached count. */
    reasoningTokens?: number;
    cost?: number; currency?: string;
  };
}
export interface AuxiliaryError extends Error {
  code: 'invalid_request' | 'model_unavailable' | 'failed' | 'cancelled';
}
/** Provider-owned process service; credentials and request lifetime stay with Provider. */
export interface AuxiliaryModel {
  /** Resolving or rejecting cancelled cancels and joins the original model call. */
  complete(call: { model: ModelId; request: AuxiliaryRequest }, cancelled?: () => Promise<void>): Promise<AuxiliaryReply>;
}

export type ModalityTargetReply =
  { status: 'resolved'; target: ModelId } |
  { status: 'unavailable'; reasons: { dimension: string; detail: string }[] };
export interface ModalityTargetError extends Error {
  code: 'invalid_params' | 'modality_unknown_operation' | 'modality_failed' |
    'operation_interrupted' | 'service_reloading';
}
/** Process service `modalityTargets`; current operation selection, without credentials or execution. */
export interface ModalityTargets {
  resolve(request: { category: string; operation: string; model?: ModelId | null }): Promise<ModalityTargetReply>;
}

export type PermissionPreset = 'read-only' | 'workspace-write' | 'full-access';
/** Session command-rule read; the owner checks the current user source on each call. */
export type CommandRule = { command: string; match: 'exact' | 'prefix'; decision: 'allow' | 'deny' }
export type CommandRulesReply = { rules: CommandRule[] }
export interface CommandRulesError extends Error { code: 'unavailable' | 'cancelled' }
export interface CommandRules { read(request: Record<string, never>): Promise<CommandRulesReply> }

/** Model configuration's existing serde form; numeric fields are finite numbers. */
export interface ModelConfig {
  temperature?: number; maxTokens?: number; topP?: number; topK?: number;
  stopSequences?: string[];
  reasoning?: {
    enabled?: true; effort?: string; budgetTokens?: number;
    summary?: 'auto' | 'concise' | 'detailed'; includeThoughts?: boolean; interleavedThinking?: boolean;
  };
  providerOptions?: {
    openai?: {
      reasoningEffort?: string; serviceTier?: string; summary?: 'auto' | 'concise' | 'detailed';
      interleavedThinking?: boolean;
    };
    'openai-compatible'?: {
      reasoningEffort?: string; serviceTier?: string; interleavedThinking?: boolean;
      thinking?: { type?: 'enabled' | 'disabled' }; enable_thinking?: boolean;
    };
    anthropic?: {
      effort?: string; thinking?: { type?: 'adaptive' | 'enabled' | 'disabled'; budgetTokens?: number };
      sendReasoning?: boolean; interleavedThinking?: boolean;
    };
    gemini?: { thinkingConfig?: { thinkingBudget?: number; includeThoughts?: boolean } };
    deepseek?: EffortProviderOptions; moonshotai?: EffortProviderOptions; xai?: EffortProviderOptions;
    zenmux?: EffortProviderOptions; aihubmix?: EffortProviderOptions;
  };
}
export interface EffortProviderOptions { reasoningEffort?: string; interleavedThinking?: boolean }

/** One source's loaded prompt value, using the same target identity. */
export interface PromptEntry {
  source: string; version: string; body: string;
  scope: import('./branch-services.js').PromptScope;
  delivery: 'system' | 'context' | 'state'; order: number;
}
export interface PromptLoadError extends Error { code: 'failed' | 'cancelled' }
export interface PromptHost {
  /** Explicit cancellation or caller-row unload notifies the source and waits for its own result.
   * Abandoning the returned Promise does not cancel an accepted load. */
  load(target: import('./branch-services.js').PromptSourceTarget, cancelled: () => Promise<void>): Promise<PromptEntry | null>;
}
export interface PromptRegistrationError extends Error { code: 'failed' }
export interface PromptRegistry {
  names(): string[];
  /** Withdrawal removes future lookups; accepted loads keep their original callback. */
  register(name: string, load: (target: import('./branch-services.js').PromptSourceTarget, cancelled: () => Promise<void>) => Promise<PromptEntry | null>): Promise<void>;
}

export interface PermissionPolicy {
  preset: PermissionPreset;
  allowedTools: string[];
  disallowedTools: string[];
}

export interface ImageSource {
  type: 'base64' | 'url';
  mediaType: string;
  data?: string;
  url?: string;
}
export type ContentBlock = { type: 'text'; text: string } | { type: 'image'; source: ImageSource };
export interface SessionRef { sessionId: string; branchId: string }
/** A managed worktree to create with the Session (worktree management §3).
 * Advertised as `sessions.launch.worktree`; absent: an ordinary Session. */
export interface WorktreeRequest {
  base?: string;
  branch?: string;
}
/** The created worktree (worktree management §3): the task id, the management
 * root and the directory the Session starts in. */
export interface WorktreeReply {
  taskId: string;
  root: string;
  cwd: string;
  repository: string;
  base: string;
  branch: string;
}
export interface LaunchRequest {
  agent?: string | null;
  cwd: string;
  model?: ModelId | null;
  policy?: Partial<PermissionPolicy> | null;
  starter?: SessionRef | null;
  /** The class of work the launch runs; absent: the caller's own, `internal`. */
  origin?: 'automation' | null;
  /** The label a picker shows before the first input; SessionTitle writes it. */
  title?: string | null;
  /** Create the Session in a new managed worktree instead of `cwd`; requires
   * `sessions.launch.worktree`. */
  worktree?: WorktreeRequest;
}
export interface LaunchReply {
  sessionId: string;
  policy: PermissionPolicy;
  /** The created worktree; omitted when the request did not ask for one. */
  worktree?: WorktreeReply;
}
export interface RawBlock {
  text: string;
  key?: { use: 'live' | 'retire'; name: string } | null;
}
export interface InputWrite extends SessionRef {
  mode: 'queue' | 'steer' | 'append';
  submissionId: string;
  rawBlocks: RawBlock[];
  content: ContentBlock[];
}
export interface InputReceipt { inputId: string; sequence: number }
export interface LookupRequest extends SessionRef { submissionId: string }
export type LookupReply = { status: 'accepted'; inputId: string; sequence: number } | { status: 'unknown' };
export interface HistoryRequest extends SessionRef {
  after?: number | null;
  through?: number | null;
  atLeast?: number | null;
  limit?: number | null;
}

/** Native C5 bridge events retain number | bigint sequences. */
export type BranchEvent = Omit<AgentEvent, 'sequence' | 'data'> & {
  sequence: number | bigint;
  data: Record<string, unknown>;
};
/** The services Dim's Rust types serve to other languages, by name:
 * `ctx.use<Services['name']>('name')`. */
export type {
  AgentErrorThrown, BranchLifecycle, Dispatcher, PartRegistry, PartScope, ProcessActivity, Services, SessionIdentity, SessionLifecycle,
  TimelineIdentity, TreeCommands, PermissionState, SessionPermissions, SessionsErrorThrown,
  WorkflowFiles, WorkflowFileEntry, WorkflowRoot, WorkflowFilesErrorThrown,
  UiResources, PublishFile, ResourceRef, ResourceErrorThrown,
} from './branch-services.js';
export interface HistoryPage { through: number; events: AgentEvent[] }
export interface WithdrawRequest extends SessionRef { inputId: string; reason?: string | null }
/** Whether an input is still pending on the branch (session access §2): from
 * its write until consumption or deletion. An id the branch does not hold is
 * not pending. */
export interface PendingRequest extends SessionRef { inputId: string }
export type WithdrawReply = { status: 'deleted'; sequence: number } | { status: 'notHeld' };
export interface StopRequest extends SessionRef { reason?: string | null }
export interface SequenceReply { sequence: number }
export type SessionsErrorCode = import('./branch-services.js').SessionsErrorThrown['code'];
export interface SessionsError { code: SessionsErrorCode; message: string; retryable: boolean }
export type SessionsResult<T> = { ok: true; value: T } | { ok: false; error: SessionsError };

/** Process service `sessions`; these methods are not terminal RPC methods. */
export interface Sessions {
  launch(request: LaunchRequest): Promise<SessionsResult<LaunchReply>>;
  input(request: InputWrite): Promise<SessionsResult<InputReceipt>>;
  lookup(request: LookupRequest): Promise<SessionsResult<LookupReply>>;
  /** Whether the id still exists: absent means deleted, and a closed,
   * restorable Session exists. Reads only; never acquires or restores a
   * Session. */
  exists(request: { sessionId: string }): Promise<SessionsResult<{ exists: boolean }>>;
  history(request: HistoryRequest): Promise<SessionsResult<HistoryPage>>;
  withdraw(request: WithdrawRequest): Promise<SessionsResult<WithdrawReply>>;
  pending(request: PendingRequest): Promise<SessionsResult<{ pending: boolean }>>;
  stop(request: StopRequest): Promise<SessionsResult<SequenceReply>>;
  idle(request: SessionRef): Promise<SessionsResult<SequenceReply>>;
  delete(request: { sessionId: string }): Promise<SessionsResult<Record<string, never>>>;
}

/** One type an `eventHub` source declares (design A.1). */
export interface EventHubType {
  type: string;
  description: string;
  mode: 'queue' | 'steer' | 'append';
}
/** `eventHub.declare`: a source whose declaration lasts while the calling row
 * is loaded (design A.1). */
export interface EventHubDeclaration {
  source: string;
  guide: string;
  types: EventHubType[];
}
/** One explicit target of a publish (design A.2). */
export interface EventHubTarget {
  sessionId: string;
  branchId?: string;
  mode?: 'queue' | 'steer' | 'append';
}
/** `eventHub.publish` (design A.2). */
export interface EventHubPublishRequest {
  source: string;
  type: string;
  /** At most 128 bytes; absent: a UUID. Retrying with the same id is idempotent per branch. */
  id?: string;
  title?: string;
  body: string;
  attributes?: Record<string, string>;
  to?: EventHubTarget[];
}
/** One target branch's outcome of a publish (design 2.3). */
export type EventHubDelivery =
  | { status: 'delivered'; sessionId: string; branchId: string; subscriptionIds: string[];
      inputId: string; sequence: number; truncated?: boolean; repeated?: boolean }
  | { status: 'gone'; sessionId: string; branchId: string; subscriptionIds: string[];
      code: 'branch_missing' | 'branch_ended' }
  | { status: 'failed'; sessionId: string; branchId: string; subscriptionIds: string[];
      code: 'session_closed' | 'reloading' | 'rejected' | 'ambiguous' |
        'publication_unknown' | 'transport' | 'failed'; message: string };
/** `publish`'s reply, after every target has an outcome (design A.2). */
export interface EventHubPublishReply { eventId: string; deliveries: EventHubDelivery[] }
/** The codes the `eventHub` service throws (design Appendix C): its own and
 * the `sessions` codes it answers unchanged. */
export type EventHubErrorThrown = import('./branch-services.js').RustError<'EventHubError',
  'invalid_params' | 'invalid_event' | 'invalid_declaration' | 'duplicate_source' |
  'no_caller_row' | 'unknown_source' | 'unknown_type' | 'too_large' | 'storage' |
  'transport' | 'failed' | 'session_closed' | 'branch_missing' | 'branch_ended' |
  'reloading' | 'rejected' | 'ambiguous' | 'publication_unknown', undefined>;
/** Process service `eventHub`, provided by EventHub to producer rows. A
 * `definePlugin` row injects `eventHub` and reads it with
 * `ctx.use<EventHub>('eventHub')`: the row loads only while EventHub runs,
 * and declares at `apply`. */
export interface EventHub {
  declare(declaration: EventHubDeclaration): Promise<void>;
  publish(request: EventHubPublishRequest): Promise<EventHubPublishReply>;
}

/** `eventSubscriptions.subscribe`: the subscription the calling row creates
 * for one Session (design A.3, §3.3). */
export interface EventSubscriptionCall {
  sessionId: string;
  types: string[];
  match?: Record<string, string>;
  mode?: 'queue' | 'steer' | 'append';
  /** Defaults to `main`. */
  branchId?: string;
}
/** `eventSubscriptions.unsubscribe` (design A.3). */
export interface EventUnsubscribeCall {
  sessionId: string;
  subscriptionId: string;
}
/** Process service `eventSubscriptions`, EventHub's subscription core: a row
 * of any language manages the subscriptions of the Session it names, as
 * `eventhub.subscribe`, `eventhub.unsubscribe` and `eventhub.subscriptions`
 * do (design Appendix F). */
export interface EventSubscriptions {
  sources(): Promise<unknown>;
  subscribe(call: EventSubscriptionCall): Promise<unknown>;
  unsubscribe(call: EventUnsubscribeCall): Promise<unknown>;
  list(call: { sessionId: string }): Promise<unknown>;
}

/** The host's creation-time snapshot (`sessionConfig.get()`, `ToolCall.config`);
 * provider-owned nested objects remain JSON. */
export interface SessionConfig {
  readonly permissionPolicy: PermissionPolicy;
  readonly cwd: string;
  readonly model: Readonly<ModelId & {
    capabilitySnapshot?: JsonObject;
    config?: JsonObject;
    metadata?: JsonObject;
  }>;
  readonly systemPrompt: string;
  /** The Session's mount list; absent: every tool the process provides is declared. */
  readonly tools?: readonly string[];
  readonly collaborationRole: 'leader' | 'member';
  /** Native serde omits true; absence means interactive. */
  readonly interactive?: false;
  readonly excludedTools?: readonly string[];
  readonly language?: import('./branch-services.js').ReplyLanguage;
  readonly initialTitle?: string;
}

/**
 * A record on `session/{sessionId}`, the Session's channel on the bus: a row
 * listens with ``ctx.on(`session/${sessionId}`, listener)``. `event` carries one
 * dispatched branch event (envelope and data unchanged); the other kinds
 * belong to their owners and a row that only observes events skips them.
 */
export type SessionBroadcast =
  | { kind: 'event'; branchId: string; event: AgentEvent }
  | { kind: 'notice'; notice: { source: string; kind: string; data: JsonValue } }
  | { kind: 'fence'; recipient: string; token: number }
  | { kind: 'closed' };
export type JsonSchema = JsonObject & {
  type?: 'object' | 'array' | 'string' | 'number' | 'integer' | 'boolean' | 'null';
  properties?: Record<string, JsonSchema>;
  items?: JsonSchema;
  required?: string[];
  enum?: JsonValue[];
  description?: string;
};
export type ToolMetadata = JsonObject & {
  version?: string;
  author?: string;
  docsUrl?: string;
  sourcePluginId?: string;
  mcpServerId?: string;
  mcpOriginalToolName?: string;
};
export interface ToolDefinition {
  name: string;
  description: string;
  inputSchema: JsonSchema;
  source: 'static' | 'builtin' | 'extension' | 'dynamic';
  /** Nonempty; Permission recognises read_filesystem, write_filesystem, execute_process,
   * network and session. Other strings are unclassified. */
  risk: string;
  metadata?: ToolMetadata;
  fileChanges?: {
    preExecutePaths?: { selector: 'path' | 'affectedPaths' };
    postExecutePaths?: 'path' | 'affectedPaths';
  };
}
export interface ToolCall extends SessionRef {
  turnId: string;
  callId: string;
  arguments: JsonObject;
  config: SessionConfig;
}
export type ToolContent = string | ContentBlock[];
export interface BlobRef {
  blobId: string;
  sessionId: string;
  kind: 'text' | 'binary' | 'file' | 'stream' | 'remote';
  path: string;
  ref?: string;
  mimeType?: string;
  sizeBytes?: number;
}
export interface BackgroundTaskInfo {
  taskId: string;
  wakePolicy: 'auto' | 'manual';
  outputPath?: string;
  description?: string;
}
export type ContentMetadata =
  { protocol: 'bedrock'; signature?: string } |
  { protocol: 'bedrock-redacted' | 'anthropic-redacted'; data: string } |
  { protocol: 'anthropic'; signature: string } |
  { protocol: 'gemini'; thoughtSignature: string } |
  { protocol: 'xai-responses'; itemId: string; encryptedContent?: string } |
  { protocol: 'openai-responses'; itemId: string; encryptedContent?: string; summaryIndex?: number; phase?: string };
export type MessagePart =
  { type: 'text'; text: string; providerMetadata?: ContentMetadata; startTime?: string; endTime?: string } |
  { type: 'image'; source: ImageSource } |
  { type: 'tool_use'; id: string; name: string; input: JsonObject; providerMetadata?: ContentMetadata; startTime?: string } |
  { type: 'tool_result'; tool_use_id: string; content: ToolContent; structuredContent?: JsonObject; is_error?: boolean } |
  { type: 'thinking'; thinking: string; providerMetadata?: ContentMetadata; startTime?: string; endTime?: string };
export interface Message {
  role: 'user' | 'assistant' | 'tool_result';
  content: string | MessagePart[];
}
/** What a tool's call did, declared on the result it returns (session boundary
 * A.6). The vocabulary is closed: a kind or a field outside it is refused when
 * the bridge decodes the result. */
export type ToolEffect =
  { kind: 'fileWritten'; path: string } |
  { kind: 'commandRun'; command: string; exitCode?: number; signal?: string };
export interface ToolResult {
  content: ToolContent;
  structuredContent?: JsonObject;
  isError?: boolean;
  blobRefs?: BlobRef[];
  backgroundTask?: BackgroundTaskInfo;
  contextMessages?: Message[];
  effects?: ToolEffect[];
}
/** How a registered tool runs a call; `cancelled` settles when the call is cancelled. */
export type ToolExecute = (call: ToolCall, cancelled: Promise<void>) => Promise<ToolResult>;
/** What a registration says besides its declaration and its function. It is an
 * execution property of the bound handler, not part of the persisted
 * `ToolDefinition`. */
export interface ToolRegistrationOptions {
  /** `'concurrent'` lets this tool's calls share a step with the calls around
   * them. Absent means the tool's calls run alone, in the order the model wrote
   * them, which is what a tool whose effects are not declared needs. */
  concurrency?: 'concurrent';
}
/** Session service `toolRegistry`: a Session row registers each of its tools,
 * which stays registered until the row unloads. The declaration must name its risk. */
export interface ToolRegistry {
  register(declaration: ToolDefinition, execute: ToolExecute, options?: ToolRegistrationOptions): Promise<void>;
}

/** One entry of the input guide: the markers it explains, its position among
 * the entries (ties break by the first marker's name) and its text, verbatim. */
export interface GuideEntry {
  markers: string[];
  order: number;
  text: string;
}
/** Where an entry applies: every branch of the Session, or one branch by id. */
export type GuideScope = 'session' | { branch: string };
/** What a refused registration throws; the codes are the ones the registry
 * carries, `closed` when the effect cannot outlive its caller, and
 * `no_caller_row` for a call made outside a row. */
export interface GuideEntryError extends Error {
  code: 'invalid_entry' | 'duplicate_marker' | 'too_large' | 'closed' | 'no_caller_row';
}
/** Session service `guideEntries`: a Session row registers the entry for a
 * marker it writes, which stays registered until the row unloads. The call
 * resolves with no value; a refusal throws a `GuideEntryError`. */
export interface GuideEntries {
  register(scope: GuideScope, entry: GuideEntry): Promise<void>;
}

export interface MethodDeclaration {
  name: string;
  mutating: boolean;
  interruption: 'report' | 'drain';
  progress: boolean;
  /** Notification names inside the namespace, unique across the declaration
   * set. A row may send only names a namespace it registered declares. */
  notifications?: string[];
}
export interface MethodCall {
  callId: string;
  method: string;
  params: BridgeJsonValue;
  /** The connection that called, when a client connection made the call. */
  origin?: { connectionId: string };
}
export interface SessionMethodCall extends MethodCall {
  params: BridgeJsonObject & { sessionId: string; branchId?: string };
}
/** A method's failure as data; Rpc maps `code` to the wire error. A method may
 * instead rethrow a service error it received. */
export interface MethodError { code: string; message: string }
export type MethodReply<T = BridgeJsonValue> = { kind: 'value'; value?: T } | MethodError;
/** Checks data fields without requiring interfaces to declare an index signature. */
export type CheckedData<T> = T extends BridgeJsonValue | undefined ? T
  : T extends ((...args: never[]) => unknown) | (abstract new (...args: never[]) => unknown) ? never
  : T extends object ? keyof T extends never ? never : { [K in keyof T]: CheckedData<T[K]> } : never;
export type MethodProgress = <T>(data: T & CheckedData<T>) => void;
export type SessionMethodDeclaration = MethodDeclaration;
export type SessionMethodReply<T = BridgeJsonValue> = MethodReply<T>;
/** How a registered management namespace serves one call. */
export type MethodHandler<T = BridgeJsonValue> = (call: MethodCall, cancelled: Promise<void>, progress: MethodProgress) => Promise<MethodReply<T>>;
/** How a registered Session namespace serves one call. */
export type SessionMethodHandler<T = BridgeJsonValue> = (call: SessionMethodCall, cancelled: Promise<void>, progress: MethodProgress) => Promise<SessionMethodReply<T>>;
/** Process service `managementMethods` (protected-local v3 only): a process row
 * registers each of its namespaces, which stays registered until the row unloads. */
export interface ManagementMethods {
  register<T extends MethodReply<unknown>>(namespace: string, declarations: MethodDeclaration[], call: (call: MethodCall, cancelled: Promise<void>, progress: MethodProgress) => Promise<T & CheckedData<T>>): Promise<void>;
}
/** Session service `sessionMethods`: a Session row registers each of its
 * namespaces, which stays registered until the row unloads. */
export interface SessionMethods {
  register<T extends SessionMethodReply<unknown>>(namespace: string, declarations: SessionMethodDeclaration[], call: (call: SessionMethodCall, cancelled: Promise<void>, progress: MethodProgress) => Promise<T & CheckedData<T>>): Promise<void>;
}

export type CallChannelErrorCode = 'undeclared' | 'gone' | 'too_large';
/** The error `notify` rejects with: `undeclared` names a notification no
 * namespace that served the connection declared, `gone` a connection that
 * has closed, a load that has unloaded, or one the row received no call
 * from, and `too_large` one over the frame limit. */
export interface CallChannelError extends Error { code: CallChannelErrorCode }
/** Process service `callChannel`, provided by App Server (session boundary A.2): the
 * notifications a row sends to a connection that called one of its
 * namespaces, and the wait for that connection to close. `connectionId` is
 * the `origin.connectionId` of a call, and stays valid until the connection
 * closes. */
export interface CallChannel {
  /** The notification is written to that connection only, after the reply of
   * the call named by `callId`, the `MethodCall.callId` of the work it
   * reports. Once that reply is written, a notification with its id is
   * written directly. */
  notify(request: { connectionId: string; callId: string; method: string; params: JsonObject }): Promise<Record<string, never>>;
  /** Resolves when the connection closes or the Rpc load that admitted the
   * call unloads. An id the row never received a call from rejects `gone`. */
  closed(request: { connectionId: string }): Promise<void>;
}

/** Process data face `sessionFiles`; errors reject with SessionFileError {code,message}. */
export interface SessionFiles {
  put(request: { sessionId: string; name: string; content: string }): Promise<StoredSessionFile>;
  import(request: { sessionId: string; name: string; source: string }): Promise<StoredSessionFile>;
  read(request: { sessionId: string; fileId: string; offset: number }): Promise<{ data: string; eof: boolean }>;
  register(request: { sessionId: string; path: string }): Promise<Record<string, never>>;
  owns(request: { sessionId: string; path: string }): Promise<{ owned: boolean }>;
  owned_paths(request: { sessionId: string }): Promise<{ paths: string[] }>;
}
export interface StoredSessionFile { fileId: string; path: string; bytes: number }

/** Process data face owned by the managed provider; credentials remain with the owner. */
export interface ManagedSearch {
  /** Rejects with unavailable or search_failed; abandoning the call cancels the request. */
  search(request: { query: string; count: number }): Promise<{
    results: { title: string; url: string; snippet: string; publishedAt?: string }[]
  }>
}

export interface WorktreeDirectory { repository: string; root: string }
export interface TimelineReadScope {
  key: { sessionId: string; branchId: string };
  through: number;
  segments: { branchId: string; from: number; through: number; storedThrough: number; tail: AgentEvent[] }[];
}
export type StoredAgentProfile = {
  name: string; description: string; systemPrompt: string; tools?: string[]; model?: ModelId;
};
export type StoredProviderSelection = { provider_id: string; model_id: string; version: number };
export interface SqliteStorage extends Pick<TimelineStore, "createSession" | "sessionConfig" | "branches" | "head" | "batchEnd" | "batchEndAt"> {
  commitTimelineRound(request: { batches: TimelineAppend[] }): Promise<{ sessions: { sessionId: string; result: { Ok: null } | { Err: { code: string; message: string } } }[] }>;
  readSelection(request: { scope: string }): Promise<StoredProviderSelection | null>;
  writeSelection(request: { scope: string; expectedVersion?: number | null; selection: ModelId }): Promise<StoredProviderSelection>;
  /** Plugins write through sessionFiles. Only the SessionFiles owner supplies its active generation for reserve, commit and abandon. */
  reserveFile(request: { sessionId: string; fileId: string; generation: string }): Promise<boolean>;
  commitFile(request: { sessionId: string; fileId: string; generation: string }): Promise<"owned" | "sessionGone" | "superseded">;
  abandonFile(request: { fileId: string; generation: string }): Promise<number>;
  ownsFile(request: { sessionId: string; fileId: string }): Promise<boolean>;
  ownsOutput(request: { sessionId: string; path: string }): Promise<boolean>;
  ownedPaths(request: { sessionId: string }): Promise<[string[], string[]]>;
  /** Supply a resolved absolute UTF-8 path under the Session command-output directory or a persistent shell output directory below the host execution root. */
  registerOutput(request: { sessionId: string; path: string }): Promise<boolean>;
  readPreferences(request: { providerId: string; modelId: string }): Promise<{ reasoningEffort?: string; serviceTier?: string } | null>;
  agents(request: Record<string, never>): Promise<{ id: string; profile: { Ok: StoredAgentProfile } | { Err: string }; default: boolean; owner: string | null }[]>;
  owners(request: Record<string, never>): Promise<{ sessionId: string; owner: [string, number] | null; error: string | null }[]>;
  save(request: { id: string; profile: StoredAgentProfile; default: boolean; owner?: string | null }): Promise<void>;
  delete(request: { id: string }): Promise<void>;
  members(request: Record<string, never>): Promise<[string, string][]>;
  ledMembers(request: { leader: string }): Promise<string[]>;
  ownedAgents(request: { owner: string }): Promise<string[]>;
  memberSessions(request: { agent: string }): Promise<string[]>;
  agentOwner(request: { agent: string }): Promise<string | null>;
  unfinishedOwned(request: Record<string, never>): Promise<{ incomplete: string[]; orphaned: string[] }>;
  replaceUser(request: { source: string; expectedVersion: number | bigint; decision: { kind: 'write'; document: string } | { kind: 'reject'; field: string } }): Promise<{ version: number | bigint }>;
  importUserIfAbsent(request: { source: string; document: string; importedRevision: string }): Promise<{ version: number | bigint; document: string }>;
  readUser(request: { source: string }): Promise<{ version: number | bigint; document: string } | null>;
  storageReport(request: Record<string, never>): Promise<{ databaseBytes: number; walBytes: number; reclaimableBytes: number }>;
}

/** Process repository operations for trusted plugins; ids are not scoped to the caller. Create/fork/delete reject ids that are not one normal path component. */
export interface Store {
  query(request: { scope: TimelineReadScope; query: import('./branch-services.js').QuerySpec; page?: import('./branch-services.js').QueryPage | null }): Promise<{ result: import('./branch-services.js').QueryOutput; next: import('./branch-services.js').QueryCursor | null }>;
  /** Stored page: database sequence and closed=false; the lifecycle owner supplies live enrichment. */
  catalog(request: { request: { limit?: number; cursor?: string; query?: string; cwd?: string | string[]; withPreview?: boolean; withInternal?: boolean; withAcpSessions?: boolean; withAutomationSessions?: boolean; includeClosed?: boolean }; agent?: string | null; version: number; hosts: string[]; closed: string[] }): Promise<{ sessions: import('./client-protocol.js').SessionSummary[]; nextCursor?: string }>;

  addGrant(request: { sessionId: string; path: string; kind: 'file' | 'dir'; mode: 'read' | 'write' }): Promise<{}>;
  removeGrant(request: { sessionId: string; path: string }): Promise<{}>;
  grants(request: { sessionId: string }): Promise<{ grants: { path: string; kind: 'file' | 'dir'; mode: 'read' | 'write' }[] }>;
  setSessionLanguage(request: { sessionId: string; language: import('./branch-services.js').ReplyLanguage | null }): Promise<{}>;
  creationRecord(request: { sessionId: string }): Promise<{ origin: 'client' | 'acp' | 'internal' | 'unknown' | 'automation'; leaderSessionId: string | null }>;
  owner(request: { sessionId: string }): Promise<{ agentId: string | null }>;
  hasRecords(request: { sessionId: string }): Promise<{ exists: boolean }>;
  /** Storage removal after the lifecycle owner has closed the Session and settled its producers. */
  delete(request: { sessionId: string }): Promise<{ removed: boolean }>;
  /** Prepared row copy after role/cut/context/rest validation; configText is source text with initialTitle removed. */
  fork(request: { sourceSessionId: string; sessionId: string; at: number; configText: string }): Promise<{}>;
  sessionConfigText(request: { sessionId: string }): Promise<{ configText: string | null }>;
  sessionConfig(request: { sessionId: string }): Promise<{ config: SessionConfig | null }>;
  /** Prepare rows after admission, supported-version/configuration checks and protocol feature restrictions; caller initializes/publishes or rolls back. */
  createOwnedSession(request: { sessionId: string; config: SessionConfig; agentId: string; protocolVersion: number; leaderSessionId: string | null; origin: 'client' | 'acp' | 'internal' | 'unknown' | 'automation'; worktree: (WorktreeDirectory & { subtree: string }) | null }): Promise<{}>;
  worktreeDirectories(request: {}): Promise<{ directories: (WorktreeDirectory & { lastUsedAtMs: number | null })[] }>;
  sessionsUnderWorkspace(request: { root: string }): Promise<{ sessionIds: string[] }>;
  removeWorktreeDirectory(request: WorktreeDirectory): Promise<{ removed: boolean }>;
  worktreeUsages(request: WorktreeDirectory): Promise<{ usages: { sessionId: string; subtree: string }[] }>;
  /** Callback runs under the native ownership mutex: do not await createOwnedSession, fork, delete, removeWorktreeDirectory, removeWorktree or worktreeAssociations.associate from it. */
  removeWorktree(request: WorktreeDirectory, remove: (path: string) => Promise<void>): Promise<{ removed: boolean }>;
}

/** Session Timeline storage binding; resource-faces §5. Native append stays beside its state owner. */
export interface StoredTimelineEvent {
  event: import('./client-protocol.js').AgentEvent;
  trace: TimelineTrace | null;
  batchEnd: number;
}
export interface TimelineTrace {
  event_id: string;
  original: Omit<import('./client-protocol.js').AgentEvent, 'id' | 'sequence' | 'create_at'>;
  steps: { handler: string; result?: Omit<import('./client-protocol.js').AgentEvent, 'id' | 'sequence' | 'create_at'> }[];
}
export interface StoredTimelineBranch {
  info: { id: string; parent?: string; branch_point?: number; create_anchor?: number };
  sequence: number;
}
export interface TimelineRecoverySnapshot {
  sequence: number;
  event_id: string;
  interpreter: string;
  state: string;
}
export interface TimelineStoreCompletion {
  subscriptionId: string;
  batchId: string;
  branchId: string;
  through: number;
  result: 'committed' | { code: string; message: string };
}
export type TimelineAppend = { sessionId: string; branchId: string; batchId: string; events: StoredTimelineEvent[] };
export interface TimelineStore {
  /** Synchronous admission is host-local; callers across a link are rejected. */
  append(request: TimelineAppend): { accepted: true };
  /** Registration completes before return; observe later admissions, without replay of earlier batches. */
  commits(request: { sessionId: string }, listener: (completion: TimelineStoreCompletion) => void | Promise<void>): Promise<{ subscriptionId: string }>;
  closeCommits(request: { subscriptionId: string }): Promise<{}>;
  /** Prepare the backend record; the lifecycle owner initializes and publishes the Agent Session. */
  createSession(request: { sessionId: string; config: SessionConfig }): Promise<{}>;
  sessionConfig(request: { sessionId: string }): Promise<{ config: SessionConfig | null }>;
  /** Drain accepted storage writes; does not close the Agent Session or its admission. */
  releaseSession(request: { sessionId: string }): Promise<{}>;
  /** Undo a prepared creation after failed initialization and stopped producers; not general Session deletion. */
  rollbackSession(request: { sessionId: string }): Promise<{}>;
  branches(request: { sessionId: string }): Promise<{ branches: StoredTimelineBranch[] }>;
  head(request: { sessionId: string; branchId: string }): Promise<{ sequence: number }>;
  readPage(request: { sessionId: string; branchId: string; after: number; limit: number; through: number }): Promise<{ events: StoredTimelineEvent[] }>;
  readSegments(request: { sessionId: string; segments: { branchId: string; from: number; through: number }[] }): Promise<{ segments: StoredTimelineEvent[][] }>;
  query: Store['query'];
  trace(request: { sessionId: string; branchId: string; sequence: number }): Promise<{ trace: TimelineTrace | null }>;
  batchEnd(request: { sessionId: string; branchId: string; sequence: number }): Promise<{}>;
  batchEndAt(request: { sessionId: string; branchId: string; sequence: number }): Promise<{ sequence: number }>;
  saveSnapshot(request: { sessionId: string; branchId: string; snapshot: TimelineRecoverySnapshot }): Promise<{}>;
  latestSnapshot(request: { sessionId: string; branchId: string; through: number; interpreter: string }): Promise<{ snapshot: TimelineRecoverySnapshot | null }>;
}

export type FsEntryName = string | { Unix: number[] } | { Windows: number[] };
/** Directory operations and pinned reads on the filesystem owner; see resource-faces §4.1. */
export interface FsHost {
  stat(request: { path: string }): Promise<{ kind: 'file'; size: number } | { kind: 'directory' | 'other' | 'missing' }>;
  replace(request: { path: string; bytes: string; permissions?: 'preserve' | 'private' | 'privateReadOnly'; leaseId?: string }): Promise<Record<string, never>>;
  backup(request: { leaseId: string; suffix: string; permissions?: 'preserve' | 'private' }): Promise<Record<string, never>>;
  lock(request: { path: string }): Promise<{ leaseId: string }>;
  unlock(request: { leaseId: string }): Promise<Record<string, never>>;
  readBytes(request: { path: string; limit: number; leaseId?: string }): Promise<{ size: number; bytes: string }>;
  createDirectory(request: { path: string }): Promise<Record<string, never>>;
  list(request: { path: string }): Promise<{ entries: { name: FsEntryName; kind: 'file' | 'directory' | 'other' }[] }>;
  remove(request: { path: string } | { parent: string; name: FsEntryName }): Promise<Record<string, never>>;
  openRead(request: { path: string }): Promise<{ readId: string; size: number }>;
  readAt(request: { readId: string; offset: number; limit: number }): Promise<{ bytes: string }>;
  closeRead(request: { readId: string }): Promise<Record<string, never>>;
}

/** One enabled Skill in the current Session view. */
export type SkillSummary = { id: string; name: string; description: string; userInvocable: boolean };
/** Branch service backed by its Session Skills owner. */
export interface SkillsBranch {
  list(request: Record<string, never>): SkillSummary[];
}

export type ApprovalDestination = {
  branchId: string;
  delegationId: string;
  agentId: string;
  interactive: boolean;
  approvalTools: boolean;
};
/** Session service owned by Collaboration; reads an active delegation without changing it. */
export interface CollaborationSession {
  approvalDestination(request: { memberSessionId: string }): ApprovalDestination | null;
}

/** Process MCP owner operations for PluginManager content; credentials and connections stay private. */
export interface McpService {
  register(request: { plugin: string; dir: string; document: JsonValue }): Promise<null>;
  withdraw(request: { plugin: string }): Promise<null>;
  plugins(request: Record<string, never>): Promise<string[]>;
  refusal(request: { plugin: string }): [string, string | null] | null;
}

export type SkillIssue =
  | { kind: 'invalidManifest'; path: string; reason: string }
  | { kind: 'unreadable'; path: string; reason: string }
  | { kind: 'rootUnreadable'; root: string; reason: string }
  | { kind: 'scanLimitReached'; root: string };
/** Process Skills owner operations for plugin content; discovery state stays private. */
export interface SkillsCatalog {
  register(request: { plugin: string; root: string }): Promise<SkillIssue[]>;
  withdraw(request: { plugin: string }): Promise<null>;
  plugins(request: Record<string, never>): Promise<string[]>;
  issues(request: { plugin: string }): Promise<SkillIssue[]>;
}

/** The existing user override; null keeps the consumer's factory or built-in text. */
export interface BasePromptSettingsHost {
  base(request: Record<string, never>): Promise<string | null>;
}
export interface SkillsInstructionsSettingsHost {
  instructions(request: Record<string, never>): Promise<string | null>;
}
export interface SummaryPromptSettingsHost {
  systemPrompt(request: Record<string, never>): Promise<string | null>;
}

/** Hooks-owned plugin content registration; see Hooks A.2. */
export interface HooksService {
  register(request: { plugin: string; dir: string; document: JsonValue }): Promise<null>;
  withdraw(request: { plugin: string }): Promise<null>;
  plugins(request: Record<string, never>): Promise<string[]>;
  refusal(request: { plugin: string }): [error: string, reason: string | null] | null;
}

/** The ToolRegistry owner's read-only source status view. */
export interface ToolSourceRejection { version: string; reason: string }
export interface ToolSourceStatus { adopted: string | null; rejected: ToolSourceRejection | null }
export type ToolSourcesState = Record<string, ToolSourceStatus>
export interface ToolSourcesView {
  get(request: Record<string, never>): ToolSourcesState
  observe(request: Record<string, never>, onState: (state: ToolSourcesState) => void | Promise<void>): Promise<void>
}
