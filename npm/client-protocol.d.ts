/**
 * App Server protocol v3: the methods and notifications the web workspace uses.
 *
 * Hand-written from `crates/dim-app-server/src/protocol.rs` and
 * `tree_protocol.rs`. Values of these types come only from the readers in
 * `read.ts`; the rest of the app uses them as they are.
 */


/** The Session's main line; other branches are its lines. */

export type ModelId = { providerId: string; modelId: string }

export type SessionTitle = {
  title: string | null
  titleSource: "auto" | "manual" | null
  titleRevision: number
}

export type HelloResult = {
  instanceId: string
  serverVersion: string
  protocolVersion: number
  /** Methods and capabilities this connection may use (v3 always sends the list). */
  features: string[]
}

/** `session.summaries`: SessionInfo flattened with the summary fields. */
export type SessionSummary = SessionTitle & {
  sessionId: string
  cwd: string
  model: ModelId
  /** Unix milliseconds. */
  createdAt: number
  running: boolean
  sequence: number
  preview?: string
  origin?: "client" | "acp" | "internal" | "unknown"
  parentSessionId?: string
  /** Unix milliseconds of the last main-branch event. */
  updatedAt: number
}

export type CreateResult = { sessionId: string; model: ModelId; sequence: number }

export type HistoryPage = {
  nextBefore?: number
  turnStart?: number
  events: SessionEvent[]
  through: number
  nextAfter: number
  done: boolean
}

export type ModelUpdateResult = { model: ModelId; outcome: string }

/** `provider.list` entry; the reply carries no credentials. */
export type ProviderSummary = {
  providerId: string
  displayName: string
  enabled: boolean
  credentialConfigured: boolean
}

export type ProviderModel = {
  id: string
  name: string
  /** Set when the model cannot be selected right now. */
  unavailableReason?: string
  /** The reasoning effort values the model accepts, when it advertises reasoning preferences. */
  reasoningEfforts?: string[]
}

export type ProviderCatalog = {
  models: ProviderModel[]
  disabledModelIds: string[]
}

export type ApprovalDecision = "allow" | "deny"

export type BranchSource =
  | { kind: "tool"; turnId: string; requestId: string; callId: string }
  | { kind: "host"; operationId: string }

export type ToolResult = {
  /** Text the model read; image blocks are dropped at the boundary. */
  text: string
  isError: boolean
  /** The edit tool's display diff (`structuredContent.diff`). */
  diff?: string
}

/** The Timeline facts the projection folds; every other kind is `other`. */
export type RawBlock = { text: string; key?: string; retire: boolean }

/** A user message as the server holds it: content blocks forwarded unchanged to `session.input.withdraw`. */
export type UserInput = { content: Array<Record<string, unknown>> }

/** A base64 image content block, as the server's `ContentBlock::Image` reads it. */
export type ImageBlock = { type: "image"; source: { type: "base64"; mediaType: string; data: string } }

/** What a composer draft adds to its text: images as content blocks, and pasted texts stored as Session attachments. */
export type DraftAttachments = { images: ImageBlock[]; pastes: string[] }

/** A file or directory the `@` mention picker offers: its path under the workspace and its absolute path. */
export type FileCandidate = { relativePath: string; absolutePath: string; dir: boolean }

/** The session permission presets, named as the server's `PermissionPreset` serializes them. */
export type PermissionPreset = "read-only" | "workspace-write" | "full-access"

/** The preset and explicit tool exceptions of `main`. */
export type PermissionPolicy = {
  preset: PermissionPreset
  allowedTools: string[]
  disallowedTools: string[]
}

/** `session.permission.get`: the policy of `main` and the version a change must expect. */
export type PermissionState = { version: number; policy: PermissionPolicy }

/** A message the server holds in the queue of a running branch. */
export type QueuedInput = { id: string; text: string; skills?: string[]; hooks?: number; user: UserInput }

/** `session.input.withdraw`: the queued input was removed, or the server had already adopted it. */
export type WithdrawResult =
  | { status: "deleted"; warnings?: SubmitWarning[] }
  | { status: "notHeld"; warnings?: SubmitWarning[] }

/** The Timeline facts the projection folds; every other kind is `other`. */
export type EventContent =
  | { kind: "input"; text?: string; user?: UserInput; skills?: string[]; hooks?: number; blocks: RawBlock[] }
  | { kind: "source-input"; text: string; mode?: string; blocks: RawBlock[] }
  | { kind: "input-consumed"; inputId: string }
  | { kind: "input-applied"; inputId: string }
  | { kind: "input-deleted"; inputId: string }
  | { kind: "permission-changed"; policy: PermissionPolicy }
  | { kind: "branch-start" }
  | { kind: "session-forked" }
  | { kind: "branch-create"; branchId: string; at: number | undefined; prompt: string; source: BranchSource }
  | { kind: "branch-create-failed"; branchId: string; status: "failed" | "cancelled"; error: string }
  | { kind: "branch-end"; source: BranchSource; ancestor?: string }
  | { kind: "branch-ended"; branchId: string; source: BranchSource; ancestor?: string }
  | { kind: "turn-end"; outcome: "completed" | "failed" | "cancelled"; error?: string }
  | { kind: "model-start"; requestId: string }
  | { kind: "thinking"; requestId: string; delta: string }
  | { kind: "text"; requestId: string; delta: string }
  | { kind: "tool-use-start"; requestId: string; callId: string; name: string }
  | { kind: "tool-use"; requestId: string; callId: string; name: string; input: Record<string, unknown> }
  | { kind: "tool-call"; requestId: string; asks: AskCall[] }
  | { kind: "tool-completed"; requestId: string; callId: string; result: ToolResult }
  | { kind: "tool-result"; requestId: string; results: Array<{ callId: string; result: ToolResult }> }
  | { kind: "model-end"; requestId: string; error?: string; presentation?: ProviderPresentation; failure?: ModelFailure }
  | { kind: "context-configured"; model: ModelId }
  | { kind: "other" }

/** A provider's own wording for a failure; it wins over the kind's line. */
export type ProviderPresentation = {
  source: string
  reason: string
  serverMessage?: string
  requestId?: string
  action?: string
}

/** Why a model request failed, as the terminal fact records it. */
export type ModelFailure = {
  kind: string
  status?: number
  providerCode?: string
  requestId?: string
  reason?: string
  cause: string[]
  attempts: number
  waitedMs: number
  retryAfterMs?: number
}

export type SessionEvent = {
  id: string
  sequence: number
  /** Unix milliseconds at which the event's fact was recorded: the commit time, except a thinking block's first `step/thinking`, which carries the block's start. Order by `sequence`, not this field. */
  createAt: number
  /** v4 only: the outer call of a child tool batch. */
  parentCallId?: string
  content: EventContent
}

/** `session.branch_create`: the new line and where it was cut. */
export type BranchCreateResult = {
  branchId: string
  parentBranchId?: string
  branchPoint?: number
  createEvent?: number
  /** Work a submit check did not do; absent when it raised none. */
  warnings?: SubmitWarning[]
}


/** Model output of a request not written to the Timeline yet; each notice replaces the last. */
export type ModelProgress = {
  sessionId: string
  branchId: string
  requestId?: string
  text: string
  /** Unicode scalar values of the request's `step/text` deltas before `text`. */
  textOffset: number
  thinking: string
  /** Unicode scalar values of the request's `step/thinking` deltas before `thinking`. */
  thinkingOffset: number
  /** Unix milliseconds at which the open thinking block started; absent when no block is open. */
  thinkingStartedAt?: number
  /** The retry this request is waiting to make, while it sleeps. */
  retry?: ModelRetryProgress
}

/** A retry a request is waiting for, shown while it sleeps. */
export type ModelRetryProgress = {
  /** The attempt about to run, at least 2. */
  attempt: number
  /** The total cap on attempts. */
  maxAttempts: number
  /** Unix milliseconds at which the attempt starts. */
  retryAtMs: number
  kind: string
  /** The failure's bounded reason. */
  reason?: string
}

export type ApprovalRequest = {
  sessionId: string
  branchId: string
  requestId: string
  /** The tool call that asks, when a tool asks. */
  callId?: string
  call: { name: string; input: Record<string, unknown> }
  reason: string
}

/** One question of an `ask_user` call, as the model wrote it and the boundary checked it. */
export type AskQuestion = {
  id: string
  header?: string
  question: string
  options: Array<{ label: string; description?: string }>
  multiSelect: boolean
  allowCustom: boolean
}

/** An `ask_user` call of a `step/tool-call` that waits for the user's answer. */
export type AskCall = { callId: string; questions: AskQuestion[] }

/** A waiting question, named by the branch, the batch's `requestId` and its call. */
export type PendingQuestion = { branchId: string; requestId: string; callId: string; questions: AskQuestion[] }

/** One answer per question; `custom` is the typed text. */
export type QuestionAnswer = { questionId: string; selected: string[]; custom?: string }

/** `session.context.compact`: the run finished, or the Session skipped it with a reason. */
export type CompactResult = { status: "completed" } | { status: "skipped"; reason: string }

/** `session.context.usage` of the main line; `baselineSource` says how the input count was made. */
export type ContextUsage = {
  contextWindow: number
  outputTokens: number
  thresholdTokens: number
  estimatedInputTokens: number
  baselineSource: string
}

export type MemberApprovalDescriptor = {
  delegationId: string
  member: { agentId: string; sessionId: string; branchId: string }
  requestId: string
  call: { name: string; input: Record<string, unknown> }
  reason: string
}
export type MemberApprovalDetail = { state: "pending"; approvalId: string; descriptor: MemberApprovalDescriptor } | { state: "ended" }

export type StateNotice = { sessionId: string; branchId: string; running: boolean; stopping: boolean }
export type HistoryQuery = { after?: number; through?: number; limit: number }
export type SubmitInput = { user: UserInput; mode: "queue" | "steer"; submissionId: string }
export type JsonValue = null | boolean | number | string | JsonValue[] | JsonObject;
export interface JsonObject { [key: string]: JsonValue }

/** Current native event envelope; each event owner defines its data fields. */
export interface AgentEvent {
  id: string;
  sequence: number;
  create_at: number;
  tags?: string[];
  type: string;
  name: string;
  data: JsonObject;
  /** v4 only: the outer call of a child tool batch. */
  parent_call_id?: string;
  /** Extension events only: the producer's reading switches. */
  context_render?: boolean;
  context_state?: boolean;
}
export type TimelinePage = Omit<HistoryPage, "events"> & { events: AgentEvent[] }
/** Work a submit check did not do, for the caller to show (Hooks A.6.2). */
export type SubmitWarning = { check: string; message: string }
/** `warnings` is absent when no check raised one, and on protocol v1 and v2. */
export type SubmitResult = { event: AgentEvent; warnings?: SubmitWarning[] }
export type InterruptResult = { sequence: number }

export type Notification =
  | { method: "ui.extensions.changed"; revision: number }
  | { method: "session.event" | "session.display"; sessionId: string; branchId: string; event: SessionEvent }
  | ({ method: "session.state" } & StateNotice)
  | { method: "session.model.progress"; progress: ModelProgress }
  | { method: "approval.request"; request: ApprovalRequest }
  | { method: "approval.cancelled" | "approval.resolved"; sessionId: string; branchId: string; requestId: string }
  | { method: "memberApprovalEnded"; sessionId: string; branchId: string; approvalId: string }
  | { method: "session.title"; sessionId: string; data: SessionTitle }
  | { method: "session.closed"; sessionId: string; reason?: string }
