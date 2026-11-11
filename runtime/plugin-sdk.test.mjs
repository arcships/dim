import './permission-policy.test.mjs';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';

const root = path.resolve(import.meta.dirname, '..');
test('packed plugin service types compile the author guide and enforce the public call shapes', (t) => {
  const directory = mkdtempSync(path.join(os.tmpdir(), 'dim plugin sdk '));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const npmCli = path.join(path.dirname(process.execPath), process.platform === 'win32'
    ? 'node_modules/npm/bin/npm-cli.js' : '../lib/node_modules/npm/bin/npm-cli.js');
  const packed = spawnSync(process.execPath, [npmCli, 'pack', path.join(root, 'npm'),
    '--pack-destination', directory, '--offline', '--ignore-scripts', '--json'], { encoding: 'utf8' });
  assert.equal(packed.status, 0, packed.stderr);
  const archive = path.join(directory, JSON.parse(packed.stdout)[0].filename);
  const packageRoot = path.join(directory, 'node_modules/dimcode');
  mkdirSync(packageRoot, { recursive: true });
  const extracted = spawnSync('tar', ['-xzf', archive, '--strip-components=1', '-C', packageRoot], { encoding: 'utf8' });
  assert.equal(extracted.status, 0, extracted.stderr);
  cpSync(path.join(root, 'runtime/node_modules/@arcships/rutis'),
    path.join(directory, 'node_modules/@arcships/rutis'), { recursive: true });
  const guide = readFileSync(path.join(root, 'docs/typescript-plugin-authoring.md'), 'utf8');
  const examples = [...guide.matchAll(/```ts\n([\s\S]*?)```/g)];
  assert(examples.length > 0);
  const files = examples.map(([, source], index) => {
    const file = path.join(directory, `guide-${index}.mts`);
    writeFileSync(file, source);
    return file;
  });
  const branchFixture = path.join(directory, 'branch-part.mts');
  writeFileSync(branchFixture, readFileSync(path.join(root, 'runtime/fixtures/branch-part.ts'), 'utf8')
    .replace("'../../npm/plugin.js'", "'dimcode/plugin'").replace("'./part-row.ts'", "'./part-row.mjs'"));
  const partRow = path.join(directory, 'part-row.mts');
  writeFileSync(partRow, readFileSync(path.join(root, 'runtime/fixtures/part-row.ts'), 'utf8')
    .replace("'../../npm/plugin.js'", "'dimcode/plugin'"));
  const permissionsFixture = path.join(directory, 'session-permissions.mts');
  writeFileSync(permissionsFixture, readFileSync(path.join(root, 'runtime/fixtures/session-permissions.ts'), 'utf8')
    .replace("'../../npm/plugin.js'", "'dimcode/plugin'"));
  const workflowFixture = path.join(directory, 'workflow-files.mts');
  writeFileSync(workflowFixture, readFileSync(path.join(root, 'runtime/fixtures/workflow-files.ts'), 'utf8')
    .replace("'../../npm/plugin.js'", "'dimcode/plugin'"));
  const eventHubFixture = path.join(directory, 'event-hub-producer.mts');
  writeFileSync(eventHubFixture, readFileSync(path.join(root, 'runtime/fixtures/event-hub-producer.ts'), 'utf8')
    .replace("'../../npm/plugin.js'", "'dimcode/plugin'"));
  const calls = path.join(directory, 'calls.mts');
  writeFileSync(calls, `
import type { Settings, SettingsResult, Sessions, WorkflowFiles, WorkflowFileEntry, WorkflowFilesErrorThrown, SessionPermissions, PermissionState, PermissionPolicy, SessionsErrorThrown, SessionMetadata, SessionMetadataResult, AuxiliaryModel, AuxiliaryReply, FileScope, FileResult, ToolRegistry, ToolExecute, ToolResult, SessionMethods, SessionMethodHandler, ManagementMethods, MethodHandler, MethodDeclaration, SessionConfig, SessionBroadcast, BranchEvent, Services, AgentErrorThrown, MethodReply, CallChannel, CallChannelError, EventHub, EventHubDeclaration, EventHubDelivery, EventHubErrorThrown, EventHubPublishReply } from 'dimcode/plugin';
declare const settings: Settings;
declare const sessions: Sessions;
declare const permissions: SessionPermissions;
declare const workflowFiles: WorkflowFiles;
declare const metadata: SessionMetadata;
declare const auxiliary: AuxiliaryModel;
declare const files: FileScope;
declare const tools: ToolRegistry;
declare const execute: ToolExecute;
declare const methods: SessionMethods;
declare const management: ManagementMethods;
declare const handler: MethodHandler;
declare const sessionHandler: SessionMethodHandler;
declare const hub: EventHub;
const declarations: MethodDeclaration[] = [{ name: 'example.status', mutating: false, interruption: 'report', progress: false }];
void management.register('example', declarations, handler);
void handler({ callId: 'call', method: 'example.status', params: [1, true] }, Promise.resolve(), () => {});
// @ts-expect-error a declaration states its interruption and progress.
void management.register('example', [{ name: 'example.status', mutating: false }], handler);
const at = { sessionId: 'session', branchId: 'main' };
const permission: PermissionState = await permissions.permission(at);
const permissionVersion: number | bigint = permission.version;
const livePolicy: PermissionPolicy = permission.policy;
const defaults: PermissionPolicy = await permissions.permissionDefaults({});
const preset: 'read-only' | 'workspace-write' | 'full-access' = defaults.preset;
const allowed: string[] = livePolicy.allowedTools;
const disallowed: string[] = defaults.disallowedTools;
declare const permissionError: SessionsErrorThrown;
const permissionCode: 'rejected' | 'agent_unknown' | 'session_closed' | 'branch_missing' | 'branch_ended' | 'reloading' | 'future_sequence' | 'ambiguous' | 'publication_unknown' | 'failed' = permissionError.code;
void permissionVersion; void preset; void allowed; void disallowed; void permissionCode;
// @ts-expect-error permission reads require a branch identity.
void permissions.permission({ sessionId: 'session' });
// @ts-expect-error generated permission reads return a value without a sessions envelope.
const permissionEnvelope: { ok: boolean } = await permissions.permission(at);
// @ts-expect-error generated permission versions preserve number | bigint.
const lossyPermissionVersion: number = permission.version;
const definitionBytes: Uint8Array = await workflowFiles.read({ root: { package: 'installed' }, path: 'example.yaml' });
const definitions: WorkflowFileEntry[] = await workflowFiles.list({ root: { project: '/workspace' } });
const created: void = await workflowFiles.createDirectory({ root: { project: '/workspace' } });
const replaced: void = await workflowFiles.replace({ root: { project: '/workspace' }, path: 'example.yaml', bytes: definitionBytes });
declare const fileError: WorkflowFilesErrorThrown;
const fileCode: string = fileError.code;
void definitions; void created; void replaced; void fileCode;
// @ts-expect-error raw FileStorage root indices are not exposed.
void workflowFiles.read({ root: 0, path: 'example.yaml' });
// @ts-expect-error replacement uses native Bytes, not JSON text or a storage envelope.
void workflowFiles.replace({ root: { project: '/workspace' }, path: 'example.yaml', bytes: 'text' });
// @ts-expect-error original caller context is supplied by the generator.
void workflowFiles.list({}, { root: { project: '/workspace' } });
void settings.read({ source: 'web', scope: 'user' });
void settings.update({ source: 'web', scope: { project: 1 }, expectedRevision: 'opaque', document: { maxResults: 10 } });
const projects: SettingsResult<{ projects: { root: number; cwd: string }[] }> = settings.projects({});
void projects;
// @ts-expect-error the project scope contains the process-local root number.
void settings.read({ source: 'web', scope: { project: '/workspace' } });
// @ts-expect-error CAS requires the revision the owner read.
void settings.update({ source: 'web', scope: 'user', document: {} });
// @ts-expect-error Settings documents are JSON objects.
void settings.update({ source: 'web', scope: 'user', expectedRevision: 'opaque', document: [] });
void sessions.launch({ cwd: '/workspace', policy: { preset: 'read-only' } });
void sessions.input({ ...at, submissionId: 'notebook:unique', mode: 'queue', rawBlocks: [{ text: 'notebook' }], content: [] });
void sessions.lookup({ ...at, submissionId: 'notebook:unique' });
void sessions.history({ ...at, atLeast: 1, limit: 10 });
void sessions.withdraw({ ...at, inputId: 'input' });
void sessions.stop(at);
void sessions.idle(at);
void sessions.delete({ sessionId: at.sessionId });
const exists: Promise<{ ok: true; value: { exists: boolean } } | { ok: false; error: { code: string } }> = sessions.exists({ sessionId: at.sessionId });
void exists;
// @ts-expect-error exists reads one Session id and no other field.
void sessions.exists({ sessionId: at.sessionId, branchId: at.branchId });
const declaration: EventHubDeclaration = { source: 'example', guide: 'Events.', types: [{ type: 'example.ping', description: 'A ping', mode: 'queue' }] };
const declared: Promise<void> = hub.declare(declaration);
void declared;
const pingReply: Promise<EventHubPublishReply> = hub.publish({ source: 'example', type: 'example.ping', id: 'ping-1', body: 'hello', to: [{ sessionId: at.sessionId, branchId: 'main', mode: 'steer' }] });
void pingReply;
const delivery: EventHubDelivery = { status: 'failed', sessionId: at.sessionId, branchId: 'main', subscriptionIds: [], code: 'transport', message: 'unknown' };
void delivery;
declare const gone: Extract<EventHubDelivery, { status: 'gone' }>;
// @ts-expect-error a gone receipt carries no inputId.
void gone.inputId;
// @ts-expect-error the mode is one of the three input modes.
void hub.publish({ source: 'example', type: 'example.ping', id: 'ping-2', body: 'hello', to: [{ sessionId: at.sessionId, mode: 'replace' }] });
// @ts-expect-error a publish carries a body; only id is optional (design A.2).
void hub.publish({ source: 'example', type: 'example.ping', id: 'ping-3' });
declare const hubError: EventHubErrorThrown;
const hubCode: 'invalid_params' | 'invalid_event' | 'invalid_declaration' | 'duplicate_source' | 'no_caller_row' | 'unknown_source' | 'unknown_type' | 'too_large' | 'storage' | 'transport' | 'failed' | 'session_closed' | 'branch_missing' | 'branch_ended' | 'reloading' | 'rejected' | 'ambiguous' | 'publication_unknown' = hubError.code;
void hubCode;
void metadata.read({ sessionId: at.sessionId });
const titled: Promise<SessionMetadataResult<{ revision: number }>> = metadata.setTitle({
  sessionId: at.sessionId, expectedRevision: 0, title: { text: 'Notebook', source: 'manual' } });
void titled;
void metadata.setTitle({ sessionId: at.sessionId, expectedRevision: 1, title: null });
const titleReply: Promise<AuxiliaryReply> = auxiliary.complete({
  model: { providerId: 'provider', modelId: 'title' },
  request: { messages: [{ role: 'user', content: 'hello' }], maxOutputTokens: 32 },
});
void titleReply;
void titleReply.then(({ usage }) => {
  const extra: [number | undefined, number | undefined, number | undefined, string | undefined] = [
    usage?.cacheReadTokens, usage?.cacheWriteTokens, usage?.cost, usage?.currency,
  ];
  void extra;
});
void auxiliary.complete({ model: { providerId: 'provider', modelId: 'title' },
  request: { messages: [] } }, () => Promise.resolve());
// @ts-expect-error cancellation uses a function reference, as C5 does.
void auxiliary.complete({ model: { providerId: 'provider', modelId: 'title' }, request: { messages: [] } }, new AbortController().signal);
// @ts-expect-error the caller selects the model explicitly.
void auxiliary.complete({ request: { messages: [] } });
// @ts-expect-error the binding accepts one call object, not positional arguments.
void auxiliary.complete({ providerId: 'provider', modelId: 'title' }, { messages: [] });
// @ts-expect-error an omitted title is not a clear.
void metadata.setTitle({ sessionId: at.sessionId, expectedRevision: 1 });
// @ts-expect-error a title's source is auto or manual.
void metadata.setTitle({ sessionId: at.sessionId, expectedRevision: 1, title: { text: 'x', source: 'user' } });
const quiesced: null = files.quiesce();
const closed: Promise<null> = files.close();
const transaction: Promise<FileResult<FileResult<string | null>>> = files.withLock('notebook/state', file => file.read());
void quiesced; void closed; void transaction;
// @ts-expect-error branch identity is required.
void sessions.idle({ sessionId: 'session' });
// @ts-expect-error input mode is part of the wire contract.
void sessions.input({ ...at, submissionId: 'id', mode: 'replace', rawBlocks: [], content: [] });
// @ts-expect-error policy field spelling follows PermissionPolicy.
void sessions.launch({ cwd: '/workspace', policy: { allowed_tools: [] } });
// @ts-expect-error lease replacement accepts text, not a host storage envelope.
void files.withLock('state', file => file.replace({ version: 1, text: 'state' }));
void execute({
  ...at, turnId: 'turn', callId: 'call', arguments: { text: 'hello' },
  config: { permissionPolicy: { preset: 'read-only', allowedTools: [], disallowedTools: [] },
    cwd: '/workspace', model: { providerId: 'test', modelId: 'model' },
    systemPrompt: '', tools: ['example_echo'], collaborationRole: 'leader' },
}, Promise.resolve());
void methods.register('session.example', [{ name: 'session.example.echo', mutating: false, interruption: 'drain', progress: true }], sessionHandler);
void sessionHandler({ callId: 'call', method: 'session.example.echo', params: { sessionId: 'session' } },
  Promise.resolve(), () => {});
// @ts-expect-error a Session method call names its Session.
void sessionHandler({ callId: 'call', method: 'session.example.echo', params: {} }, Promise.resolve(), () => {});
void handler({ callId: 'call', method: 'example.status', params: [1, true],
  origin: { connectionId: 'connection' } }, Promise.resolve(), () => {});
const notificationOrigin: MethodDeclaration = { name: 'example.status', mutating: false,
  interruption: 'report', progress: false, notifications: ['example.changed'] };
void notificationOrigin;
declare const channel: CallChannel;
const notification = await channel.notify({
  connectionId: 'connection', callId: 'call', method: 'example.changed', params: { step: 'done' } });
const closedConnection: void = await channel.closed({ connectionId: 'connection' });
void notification; void closedConnection;
// @ts-expect-error a notification names its call, the declared method and JSON params.
void channel.notify({ connectionId: 'connection', method: 'example.changed' });
const channelCode: 'undeclared' | 'gone' | 'too_large' = (undefined as unknown as CallChannelError).code;
void channelCode;
const result: ToolResult = { content: [{ type: 'text', text: 'handle: owned-id' }],
  structuredContent: { handle: 'owned-id' }, backgroundTask: { taskId: 'task', wakePolicy: 'manual' },
  blobRefs: [{ blobId: 'blob', sessionId: 'session', kind: 'text', path: '/files/result' }],
  contextMessages: [{ role: 'tool_result', content: [{ type: 'tool_result', tool_use_id: 'call', content: 'done', is_error: false }] }] };
void result;
// @ts-expect-error the foreign call uses neutral identities and arguments, not the native shape.
void execute({ id: 'call', name: 'example_echo', input: {} }, Promise.resolve());
// @ts-expect-error result content is text or typed blocks, not an arbitrary object.
const invalidResult: ToolResult = { content: { handle: 'owned-id' } };
void tools.register({ name: 'example_echo', description: '', source: 'extension', inputSchema: { type: 'object' }, risk: 'session' }, execute);
// @ts-expect-error registered tools declare a Permission risk.
void tools.register({ name: 'example_echo', description: '', source: 'extension', inputSchema: { type: 'object' } }, execute);
// @ts-expect-error C4 does not implement a handoff reply.
const handoff: Awaited<ReturnType<SessionMethodHandler>> = { kind: 'handedOff' };
const record: SessionBroadcast = { kind: 'event', branchId: at.branchId, event: {
  id: 'event', sequence: 1, create_at: 1, type: 'input', name: 'applied', data: {} } };
void record;
const failure: MethodReply = { code: 'invalid_params', message: 'Invalid' };
void failure;
// @ts-expect-error Rpc chooses the wire code; a method returns its application code.
const wireFailure: MethodReply = { code: -32602, appError: 'invalid_params', message: 'Invalid' };
// @ts-expect-error an event record names its branch.
const unscopedRecord: SessionBroadcast = { kind: 'event', event: record.event };
declare const branch: Services;
const session = await branch.sessionIdentity.get();
const sessionId: string = session.sessionId;
const restored: boolean = session.restored;
const config: SessionConfig = await branch.sessionConfig.get();
const published: boolean = await branch.sessionLifecycle.activated();
const held = await branch.sessionLifecycle.hold();
const released: boolean = held === null ? false : await branch.sessionLifecycle.release(held);
void sessionId; void restored; void config; void published; void released;
// @ts-expect-error a hold is the id the row releases, not an object.
const handle: { release(): void } = await branch.sessionLifecycle.hold();
const cancelled = () => new Promise<void>(() => {});
void branch.dispatcher.listen('counter', ['input/message'], async event => {
  const sequence: number | bigint = event.sequence;
  const data: Record<string, unknown> = event.data;
  void sequence; void data;
});
void branch.dispatcher.consume('counter', ['input/message'],
  async () => 1n,
  async event => { void (event.sequence as number | bigint) });
const cutoff: number | bigint = await branch.dispatcher.committed();
const own: Awaited<ReturnType<Services['timeline']['query']>> = await branch.timeline.query({
  scope: 'branch', include_publishing: true, through: cutoff,
  query: { select: { kind: 'fields', fields: ['id', 'kind'] } },
  page: { limit: 1n },
});
const referencedEntry = await branch.timeline.query({
  scope: 'branch', through: cutoff,
  query: {
    filter: { op: 'compare', left: { kind: 'field', path: 'id' },
      comparison: 'eq', right: { kind: 'value', value: 'prompt-event' } },
    select: { kind: 'fields', fields: ['id', 'data.changes[2].entry'] },
  },
  page: { limit: 1 },
});
void referencedEntry;
void branch.treeCommands.input('main', { blocks: [{ type: 'raw', text: 'source marker' }], mode: 'queue' });
void branch.treeCommands.create('main', cutoff, { prompt: 'child', source: { kind: 'host', operation_id: 'create-id' } }, cancelled);
const info = await branch.treeCommands.info('main');
const parentCut: number | bigint | null | undefined = info.branch_point;
const active: boolean = await branch.branchLifecycle.activated();
const closing: Promise<void> = branch.branchLifecycle.closing();
const identity: { sessionId: string; branchId: string } = await branch.timelineIdentity.get();
declare const failed: AgentErrorThrown;
const code: string = failed.code;
void own; void parentCut; void active; void closing; void identity; void code;
// @ts-expect-error caller Ctx is supplied by the host, not by TypeScript.
void branch.dispatcher.listen({}, 'counter', ['input/message'], () => {});
// @ts-expect-error a handler returns void or a Promise of void.
void branch.dispatcher.listen('counter', ['input/message'], () => 'ack');
// @ts-expect-error native input uses blocks, not the sessions service's rawBlocks.
void branch.treeCommands.input('main', { rawBlocks: [{ text: 'source marker' }] });
// @ts-expect-error native source field spelling comes from Rust serde.
void branch.treeCommands.create('main', 0, { prompt: 'child', source: { kind: 'host', operationId: 'id' } }, cancelled);
// @ts-expect-error sequence values must retain their number | bigint type.
const lossyCut: number = cutoff;
`);
  const uiFixture = path.join(directory, 'ui.mts');
  writeFileSync(uiFixture, `
import type { PanelError, PanelModule, PanelServices, TimelinePage } from 'dimcode/ui';
const isClosedBinding = (error: PanelError): boolean => error.name === 'PanelError' && error.code === 'binding_closed';
void isClosedBinding;
import type { UiResources, PublishFile, ResourceRef } from 'dimcode/plugin';
declare const resources: UiResources;
const input: PublishFile = { path: '/workspace/report.txt', mediaType: 'text/plain' };
const reference: ResourceRef = await resources.publishFile(input);
await resources.withdraw(reference);
const panel: PanelModule<Required<PanelServices>> = {
  mount(root, context) {
    const link = document.createElement('a');
    link.href = context.services.resources.url(reference);
    root.append(link);
    void context.services.session.history?.read({ limit: 10 }).then((page: TimelinePage) => page.events[0]?.data);
    // @ts-expect-error panels cannot access a transport or arbitrary RPC.
    context.call('session.close', {});
    // @ts-expect-error panels cannot publish server files.
    context.services.resources.publishFile(input);
    return () => link.remove();
  },
};
void panel;
`);
  const compiler = path.join(root, 'runtime/node_modules/typescript/bin/tsc');
  const declarations = spawnSync(process.execPath, [compiler,
    '--strict', '--noEmit', '--target', 'ES2022', '--module', 'NodeNext', '--moduleResolution', 'NodeNext',
    path.join(packageRoot, 'plugin.d.ts')], { encoding: 'utf8' });
  assert.equal(declarations.status, 0, declarations.stdout + declarations.stderr);
  const checked = spawnSync(process.execPath, [compiler,
    '--strict', '--noEmit', '--target', 'ES2022', '--module', 'NodeNext', '--moduleResolution', 'NodeNext',
    ...files, calls, branchFixture, partRow, permissionsFixture, workflowFixture, eventHubFixture, uiFixture], { encoding: 'utf8' });
  assert.equal(checked.status, 0, checked.stdout + checked.stderr);
});
