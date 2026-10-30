/** Named services implemented by Dim's App Server. No runtime code is exported. */
export type FileError = 'closed' | 'exists' | 'invalid_data' | 'invalid_path' | 'internal' | 'io';
export type FileResult<T> = { ok: true; value: T } | { ok: false; error: FileError };

export interface FileLease {
  read(): Promise<FileResult<string | null>>;
  replace(text: string): Promise<FileResult<null>>;
  backup(suffix: string): Promise<FileResult<null>>;
}

export interface FileScope {
  withLock<T>(path: string, work: (file: FileLease) => T | Promise<T>, cancelled?: Promise<void>): Promise<FileResult<T>>;
  quiesce(): null;
  close(): Promise<null>;
}

export interface PluginFiles {
  open(): Promise<FileScope>;
}

/** JSON-only; rutis (0.11+) may deliver bigint in incoming tool data beyond this type. */
export type JsonValue = null | boolean | number | string | JsonValue[] | JsonObject;
export interface JsonObject { [key: string]: JsonValue }

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
export type PermissionPreset = 'read-only' | 'workspace-write' | 'full-access';
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
export interface LaunchRequest {
  agent?: string | null;
  cwd: string;
  model?: ModelId | null;
  policy?: Partial<PermissionPolicy> | null;
  starter?: SessionRef | null;
}
export interface LaunchReply { sessionId: string; policy: PermissionPolicy }
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
/** Current native event envelope; each event owner defines its data fields. */
export interface AgentEvent {
  id: string;
  sequence: number;
  create_at: number;
  tags?: string[];
  type: string;
  name: string;
  data: JsonObject;
}
/** Native C5 bridge events retain number | bigint sequences. */
export type BranchEvent = Omit<AgentEvent, 'sequence' | 'data'> & {
  sequence: number | bigint;
  data: Record<string, unknown>;
};
/** The services Dim's Rust types serve to other languages, by name:
 * `ctx.use<Services['name']>('name')`. */
export type {
  AgentErrorThrown, BranchLifecycle, Delivery, Dispatcher, PartRegistry, PartScope, Services, SessionIdentity, SessionLifecycle,
  TimelineIdentity, TreeCommands, PermissionState, SessionPermissions, SessionsErrorThrown,
  WorkflowFiles, WorkflowFileEntry, WorkflowRoot, WorkflowFilesErrorThrown,
} from './branch-services.js';
export interface HistoryPage { through: number; events: AgentEvent[] }
export interface WithdrawRequest extends SessionRef { inputId: string; reason?: string | null }
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
  history(request: HistoryRequest): Promise<SessionsResult<HistoryPage>>;
  withdraw(request: WithdrawRequest): Promise<SessionsResult<WithdrawReply>>;
  stop(request: StopRequest): Promise<SessionsResult<SequenceReply>>;
  idle(request: SessionRef): Promise<SessionsResult<SequenceReply>>;
  delete(request: { sessionId: string }): Promise<SessionsResult<Record<string, never>>>;
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
/** Session service `toolRegistry`: a Session row registers each of its tools,
 * which stays registered until the row unloads. The declaration must name its risk. */
export interface ToolRegistry {
  register(declaration: ToolDefinition, execute: ToolExecute): Promise<void>;
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
}
export interface MethodCall {
  callId: string;
  method: string;
  params: JsonValue;
}
export interface SessionMethodCall extends MethodCall {
  params: JsonObject & { sessionId: string; branchId?: string };
}
/** A method's failure as data; Rpc maps `code` to the wire error. A method may
 * instead rethrow a service error it received. */
export interface MethodError { code: string; message: string }
export type MethodReply = { kind: 'value'; value?: JsonValue } | MethodError;
export type SessionMethodDeclaration = MethodDeclaration;
export type SessionMethodReply = MethodReply;
/** How a registered management namespace serves one call. */
export type MethodHandler = (call: MethodCall, cancelled: Promise<void>, progress: (data: JsonValue) => void) => Promise<MethodReply>;
/** How a registered Session namespace serves one call. */
export type SessionMethodHandler = (call: SessionMethodCall, cancelled: Promise<void>, progress: (data: JsonValue) => void) => Promise<SessionMethodReply>;
/** Process service `managementMethods` (protected-local v3 only): a process row
 * registers each of its namespaces, which stays registered until the row unloads. */
export interface ManagementMethods {
  register(namespace: string, declarations: MethodDeclaration[], call: MethodHandler): Promise<void>;
}
/** Session service `sessionMethods`: a Session row registers each of its
 * namespaces, which stays registered until the row unloads. */
export interface SessionMethods {
  register(namespace: string, declarations: SessionMethodDeclaration[], call: SessionMethodHandler): Promise<void>;
}
