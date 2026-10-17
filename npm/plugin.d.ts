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

/** JSON-only; rutis 0.11 may deliver bigint in incoming tool data beyond this type. */
export type JsonValue = null | boolean | number | string | JsonValue[] | JsonObject;
export interface JsonObject { [key: string]: JsonValue }

/** Project roots are process-local numbers, not durable identities. */
export type SettingsScope = 'user' | { project: number };
export interface SettingsSnapshot { revision: string; document: JsonObject }
export type SettingsErrorCode = 'unknown_source' | 'invalid_scope' | 'invalid_document' |
  'user_only_field' | 'conflict' | 'unsynced' | 'failed' | 'closed' | 'cancelled';
export type SettingsResult<T> = { ok: true; value: T } |
  { ok: false; error: { code: SettingsErrorCode; message: string } };

/** Process service `settings`; TypeScript sources are declared in their C4 row. */
export interface Settings {
  read(request: { source: string; scope: SettingsScope }): Promise<SettingsResult<SettingsSnapshot>>;
  update(request: { source: string; scope: SettingsScope; expectedRevision: string; document: JsonObject }): Promise<SettingsResult<SettingsSnapshot>>;
  projects(request: Record<string, never>): SettingsResult<{ projects: { root: number; cwd: string }[] }>;
}

export type CredentialsResult = { ok: true; value: { value: string } } |
  { ok: false; error: { code: 'not_found' | 'failed' | 'cancelled'; message: string } };
/** Process service `credentials`; requires an explicitly supplied secret store. */
export interface Credentials {
  resolve(request: { owner: string; reference: string }): Promise<CredentialsResult>;
}

export interface ModelId { providerId: string; modelId: string }
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
export interface HistoryPage { through: number; events: AgentEvent[] }
export interface WithdrawRequest extends SessionRef { inputId: string; reason?: string | null }
export type WithdrawReply = { status: 'deleted'; sequence: number } | { status: 'notHeld' };
export interface StopRequest extends SessionRef { reason?: string | null }
export interface SequenceReply { sequence: number }
export type SessionsErrorCode = 'rejected' | 'agent_unknown' | 'session_closed' | 'branch_missing' |
  'reloading' | 'future_sequence' | 'ambiguous' | 'publication_unknown' | 'failed';
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

/** The host's creation-time snapshot; provider-owned nested objects remain JSON. */
export interface SessionConfig {
  readonly permissionPolicy: PermissionPolicy;
  readonly cwd: string;
  readonly model: Readonly<ModelId & {
    capabilitySnapshot?: JsonObject;
    config?: JsonObject;
    metadata?: JsonObject;
  }>;
  readonly systemPrompt: string;
  readonly tools: readonly string[];
  readonly collaborationRole: 'leader' | 'member';
  /** Native serde omits true; absence means interactive. */
  readonly interactive?: false;
}

export type JsonSchema = JsonObject & {
  type: 'object' | 'array' | 'string' | 'number' | 'integer' | 'boolean' | 'null';
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
export interface ToolResult {
  content: ToolContent;
  structuredContent?: JsonObject;
  isError?: boolean;
  blobRefs?: BlobRef[];
  backgroundTask?: BackgroundTaskInfo;
  contextMessages?: Message[];
}
/** Provide as `toolContribution:<name>`; declaration is synchronous. */
export interface ToolContribution {
  declaration(): ToolDefinition;
  execute(call: ToolCall, cancelled: Promise<void>): Promise<ToolResult>;
}

export interface SessionMethodDeclaration {
  name: string;
  mutating: boolean;
  interruption: 'report' | 'drain';
  progress: boolean;
}
export interface SessionMethodCall {
  callId: string;
  method: string;
  params: JsonObject & { sessionId: string; branchId?: string };
}
export type SessionMethodReply = { kind: 'value'; value?: JsonValue } |
  { code: number; appError: string; message: string };
/** Provide as `sessionMethods:<namespace>`; both methods are asynchronous. */
export interface SessionMethodsContribution {
  describe(): Promise<SessionMethodDeclaration[]>;
  call(call: SessionMethodCall, cancelled: Promise<void>, progress: (data: JsonValue) => void): Promise<SessionMethodReply>;
}

/** C5 keeps the native serde names, rather than the S2 input request. */
export interface InputMessageData {
  submission_id?: string;
  user?: { content: ContentBlock[] } | null;
  blocks?: { type: 'raw'; text: string; key?: string; retire?: boolean }[];
  mode?: 'queue' | 'steer' | 'append';
}
export type BranchResult<T> = { ok: true; value: T } |
  { ok: false; error: { code: 'invalid_params' | 'invalid_input' | 'unknown_branch' |
    'closed' | 'cancelled' | 'reloading' | 'failed'; message: string } };
/** Preserve rutis integers when passing native cutoffs back to the owner. */
export type TimelineSequence = number | bigint;
export interface BranchOptions {
  prompt: string;
  source: { kind: 'tool'; turn_id: string; request_id: string; call_id: string } |
    { kind: 'host'; operation_id: string };
}
export interface TimelineInfo {
  id: string;
  parent?: string;
  branch_point?: TimelineSequence;
  create_anchor?: TimelineSequence;
}
export interface BranchRef {
  readonly id: string;
  input(data: InputMessageData): BranchResult<AgentEvent>;
  /** A selected producer reference fails on unload; it never follows a successor. */
  service<T extends object>(request: { name: string }): BranchResult<T>;
}
/** Session service `treeCommands`; both Session and branch rows declare it in inject. */
export interface TreeCommands {
  get(request: { id: string }): BranchResult<BranchRef>;
  list(request: Record<string, never>): BranchResult<TimelineInfo[]>;
  /** Creation cancellation is pending the C5 contract decision. */
  branch(request: { parent: string; at: TimelineSequence; options: BranchOptions }): Promise<BranchResult<BranchRef>>;
}
/** Original branch-row references passed once to `branchPart:<part>.bind`. */
export interface BranchBinding {
  readonly identity: Readonly<SessionRef>;
  readonly dispatcher: {
    subscribe(keys: string[], callback: (event: AgentEvent) => void | Promise<void>): BranchResult<null>;
  };
  readonly timeline: {
    committed(): BranchResult<TimelineSequence>;
    history(request: { through: TimelineSequence }): Promise<BranchResult<AgentEvent[]>>;
    local(request: { start: TimelineSequence; through: TimelineSequence }): Promise<BranchResult<AgentEvent[]>>;
  };
  readonly activated: Promise<boolean>;
  readonly closing: Promise<void>;
}
export interface BranchPart {
  bind(binding: BranchBinding): Promise<null>;
}
