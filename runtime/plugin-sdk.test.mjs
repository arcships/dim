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
  const calls = path.join(directory, 'calls.mts');
  writeFileSync(calls, `
import type { Settings, SettingsResult, Credentials, CredentialsResult, Sessions, FileScope, FileResult, ToolContribution, ToolResult, SessionMethodsContribution, TreeCommands, BranchBinding, BranchRef } from 'dimcode/plugin';
declare const settings: Settings;
declare const credentials: Credentials;
declare const sessions: Sessions;
declare const files: FileScope;
declare const tool: ToolContribution;
declare const methods: SessionMethodsContribution;
declare const tree: TreeCommands;
declare const binding: BranchBinding;
declare const branch: BranchRef;
const at = { sessionId: 'session', branchId: 'main' };
void settings.read({ source: 'web', scope: 'user' });
void settings.update({ source: 'web', scope: { project: 1 }, expectedRevision: 'opaque', document: { maxResults: 10 } });
const projects: SettingsResult<{ projects: { root: number; cwd: string }[] }> = settings.projects({});
const secret: Promise<CredentialsResult> = credentials.resolve({ owner: 'web', reference: 'opaque' });
void projects; void secret;
// @ts-expect-error the project scope contains the process-local root number.
void settings.read({ source: 'web', scope: { project: '/workspace' } });
// @ts-expect-error CAS requires the revision the owner read.
void settings.update({ source: 'web', scope: 'user', document: {} });
// @ts-expect-error Settings documents are JSON objects.
void settings.update({ source: 'web', scope: 'user', expectedRevision: 'opaque', document: [] });
// @ts-expect-error credentials require an owner and an opaque reference.
void credentials.resolve({ secretRef: 'opaque' });
void sessions.launch({ cwd: '/workspace', policy: { preset: 'read-only' } });
void sessions.input({ ...at, submissionId: 'notebook:unique', mode: 'queue', rawBlocks: [{ text: 'notebook' }], content: [] });
void sessions.lookup({ ...at, submissionId: 'notebook:unique' });
void sessions.history({ ...at, atLeast: 1, limit: 10 });
void sessions.withdraw({ ...at, inputId: 'input' });
void sessions.stop(at);
void sessions.idle(at);
void sessions.delete({ sessionId: at.sessionId });
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
void tool.execute({
  ...at, turnId: 'turn', callId: 'call', arguments: { text: 'hello' },
  config: { permissionPolicy: { preset: 'read-only', allowedTools: [], disallowedTools: [] },
    cwd: '/workspace', model: { providerId: 'test', modelId: 'model' },
    systemPrompt: '', tools: ['example_echo'], collaborationRole: 'leader' },
}, Promise.resolve());
void methods.call({ callId: 'call', method: 'session.example.echo', params: { sessionId: 'session' } },
  Promise.resolve(), () => {});
void tree.get({ id: 'main' });
void tree.list({});
void tree.branch({ parent: 'main', at: 1n, options: { prompt: 'child', source: { kind: 'host', operation_id: 'operation' } } });
// @ts-expect-error the Node bridge does not send AbortSignal; the cancellation contract is pending.
void tree.branch({ parent: 'main', at: 1, options: { prompt: 'child', source: { kind: 'host', operation_id: 'operation' } } }, new AbortController().signal);
void branch.input({ submission_id: 'input', user: { content: [{ type: 'text', text: 'hello' }] }, blocks: [{ type: 'raw', text: 'runtime' }], mode: 'queue' });
const owner = branch.service<{ get(request: Record<string, never>): { through: number } }>({ name: 'goal' });
if (owner.ok) void owner.value.get({});
void binding.dispatcher.subscribe(['input/*'], async event => { void event.sequence; });
void binding.timeline.local({ start: 1, through: 2n });
void binding.timeline.history({ through: 2 });
void binding.activated.then(active => { void active; });
// @ts-expect-error callback results are unit, not a result or asynchronous work handle.
void binding.dispatcher.subscribe(['*'], () => 1);
// @ts-expect-error C5 does not expose arbitrary timeline writes.
void binding.timeline.append({});
// @ts-expect-error branch input keeps native serde names, not the S2 or terminal request.
void branch.input({ text: 'hello' });
// @ts-expect-error reconstruction requires an explicit fixed cutoff.
void binding.timeline.history({});
const result: ToolResult = { content: [{ type: 'text', text: 'handle: owned-id' }],
  structuredContent: { handle: 'owned-id' }, backgroundTask: { taskId: 'task', wakePolicy: 'manual' },
  blobRefs: [{ blobId: 'blob', sessionId: 'session', kind: 'text', path: '/files/result' }],
  contextMessages: [{ role: 'tool_result', content: [{ type: 'tool_result', tool_use_id: 'call', content: 'done', is_error: false }] }] };
void result;
// @ts-expect-error the foreign call uses neutral identities and arguments, not the native shape.
void tool.execute({ id: 'call', name: 'example_echo', input: {} }, Promise.resolve());
// @ts-expect-error result content is text or typed blocks, not an arbitrary object.
const invalidResult: ToolResult = { content: { handle: 'owned-id' } };
// @ts-expect-error contributed tools declare a Permission risk.
const missingRisk: ReturnType<ToolContribution['declaration']> = { name: 'example_echo', description: '', source: 'extension', inputSchema: { type: 'object' } };
// @ts-expect-error C4 does not implement a handoff reply.
const handoff: Awaited<ReturnType<SessionMethodsContribution['call']>> = { kind: 'handedOff' };
`);
  const compiler = path.join(root, 'runtime/node_modules/typescript/bin/tsc');
  const declarations = spawnSync(process.execPath, [compiler,
    '--strict', '--noEmit', '--target', 'ES2022', '--module', 'NodeNext', '--moduleResolution', 'NodeNext',
    path.join(packageRoot, 'plugin.d.ts')], { encoding: 'utf8' });
  assert.equal(declarations.status, 0, declarations.stdout + declarations.stderr);
  // rutis 0.10 has an optional/required Plugin inheritance conflict; check our declarations separately.
  const checked = spawnSync(process.execPath, [compiler, '--skipLibCheck',
    '--strict', '--noEmit', '--target', 'ES2022', '--module', 'NodeNext', '--moduleResolution', 'NodeNext',
    ...files, calls], { encoding: 'utf8' });
  assert.equal(checked.status, 0, checked.stdout + checked.stderr);
});
