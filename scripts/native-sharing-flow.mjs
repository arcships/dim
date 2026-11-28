import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { chmod, mkdir, mkdtemp, readFile, readdir, realpath, rm, writeFile } from 'node:fs/promises';
import { rpc } from './tui-tmux/native-rpc.mjs';
import os from 'node:os';
import path from 'node:path';
import { createInterface } from 'node:readline';
import test from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';

const windows = process.platform === 'win32';
const suffix = windows ? '.exe' : '';
const debug = path.resolve(process.env.CARGO_TARGET_DIR || 'target', 'debug');
const cli = path.resolve(process.env.DIM_SHARING_TEST_CLI || path.join(debug, 'dimcode' + suffix));
const helper = path.join(path.dirname(cli), 'dim-sharing' + suffix);
const fixture = path.join(debug, 'examples', 'sharing-shutdown-fixture' + suffix);
const model = path.resolve(import.meta.dirname, '../crates/dim-agent-aimux/examples/offline.json');

async function environment(t) {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'dim-net-'));
  const a = path.join(temp, 'a'), b = path.join(temp, 'b'), tmp = path.join(temp, 'tmp');
  const env = { ...process.env, HOME: temp, TMPDIR: tmp, DIMCODE_HOME: b, DIMCODE_UPDATE_CHECK: '0', DIMCODE_SHARING_LISTEN: '127.0.0.1:0' };
  const children = new Set();
  function start(args, program = cli, extraEnv) {
    const child = spawn(program, args, { env: { ...env, ...extraEnv }, cwd: temp, stdio: 'pipe' });
    children.add(child);
    child.on('close', () => children.delete(child));
    return child;
  }
  async function run(args, input = '', program = cli, extraEnv) {
    const child = start(args, program, extraEnv), out = [], err = [];
    child.stdout.on('data', chunk => out.push(chunk));
    child.stderr.on('data', chunk => err.push(chunk));
    child.stdin.end(input);
    const [code] = await once(child, 'close');
    return { code, out: Buffer.concat(out).toString(), err: Buffer.concat(err).toString() };
  }
  t.after(async () => {
    for (const child of children) child.kill();
    await Promise.allSettled([...children].map(child => once(child, 'close')));
    await rm(temp, { recursive: true, force: true });
  });
  await mkdir(tmp, { mode: 0o700 });
  return { temp, a, b, tmp, env, start, run };
}

test('a remembered shared link carries independent native streams, large frames and revocation', { skip: windows, timeout: 90_000 }, async (t) => {
  const { temp, a, b, tmp, env, start, run } = await environment(t);
  for (const flag of ['--help', '--version']) {
    const local = await run([flag]);
    const targeted = await run(['--target', '127.0.0.1:1', flag]);
    assert.deepEqual(targeted, local);
  }
  const target = path.join(b, 'rust', 'daemon.sock');
  async function share(...args) {
    const result = await run(['--home', b, '--model-config', model, 'share', ...args]);
    assert.equal(result.code, 0, result.err);
    return JSON.parse(result.out);
  }
  await mkdir(path.dirname(target), { recursive: true, mode: 0o700 });
  await chmod(path.dirname(target), 0o700);
  const staging = path.join(path.dirname(target), 'dimcode.sqlite.web-uploads');
  const abandoned = path.join(staging, 'upload-abandoned');
  await mkdir(abandoned, {recursive: true, mode: 0o700});
  await writeFile(path.join(abandoned, 'upload'), 'partial upload');
  const disabled = await run(['--home', a, 'share', 'disable']);
  assert.equal(disabled.code, 0, disabled.err);
  assert.equal(JSON.parse(disabled.out).enabled, false);
  await assert.rejects(readFile(path.join(a, 'rust', 'daemon.sock')), { code: 'ENOENT' });
  const replacement = path.join(temp, 'sharing-helper');
  await writeFile(replacement, '', { mode: 0o600 });
  env.DIM_TEST_SHARING_HELPER = helper;
  const server = start(['app-server', '--sharing-helper', replacement, '--home', path.dirname(target), '--model-config', model, '--no-ws', '--unix', target]);
  const diagnostics = createInterface({ input: server.stderr })[Symbol.asyncIterator]();
  async function waitForDiagnostic(message) {
    for (;;) {
      const { value, done } = await diagnostics.next();
      assert.ok(!done, 'the sharing supervisor stopped before ' + message);
      if (value.includes(message)) return;
    }
  }
  await Promise.all([once(server.stdout, 'data'), waitForDiagnostic('Remote sharing could not start')]);
  const direct = await rpc(target);
  t.after(() => direct.stream.destroy());
  await writeFile(replacement, [
    '#!/bin/sh', 'if [ ! -e "$2/restart-check" ]; then',
    '  touch "$2/restart-check"', '  exit 1', 'fi',
    'echo "starting repaired helper" >&2', 'exec "$DIM_TEST_SHARING_HELPER" "$@"', '',
  ].join('\n'));
  await chmod(replacement, 0o700);
  await waitForDiagnostic('starting repaired helper');
  assert.equal((await share('status')).enabled, false);
  const { token, addresses: [address] } = await share('enable');
  const clientArgs = ['--home', a, 'share', 'connect', address];
  assert.notEqual((await run([...clientArgs, '--', 'mcp', 'list'], 'incorrect\n')).code, 0);

  const hold = start([...clientArgs, '--', '--no-tui']);
  hold.stdin.write(token + '\n');
  let proxy;
  for (let i = 0; i < 200 && !proxy; i++) {
    for (const entry of await readdir(tmp, { withFileTypes: true })) {
      if (entry.isDirectory() && entry.name.startsWith('dim-link-')) {
        const dir = path.join(tmp, entry.name);
        if ((await readdir(dir)).includes('ready')) proxy = path.join(dir, 'rpc.sock');
      }
    }
    if (!proxy) await delay(50);
  }
  assert.ok(proxy, 'the authenticated client must publish its private socket');
  const first = await rpc(proxy), second = await rpc(proxy);
  t.after(() => { first.stream.destroy(); second.stream.destroy(); });
  const database = path.join(temp, 'keyboard.sqlite');
  const localOutput = path.join(temp, 'timeline.json');
  await writeFile(database, 'local fixture');
  for (const args of [
    ['daemon', 'vacuum', '--database', database], ['export', 'fixture-session', '--database', database, '--output', localOutput],
    ['migration', 'list'], ['update'], ['web', 'install'], ['share', 'enable'], ['--fullscreen', 'default'],
  ]) {
    const refused = await run([...clientArgs, '--', ...args]);
    assert.notEqual(refused.code, 0);
    assert.match(refused.err, /unavailable through a shared connection/, args.join(' '));
  }
  assert.equal(await readFile(database, 'utf8'), 'local fixture');
  for (const file of [localOutput, path.join(a, 'config.json'), path.join(a, 'rust', 'daemon.sock')]) {
    await assert.rejects(readFile(file), { code: 'ENOENT' });
  }
  assert.equal(first.hello.instanceId, direct.hello.instanceId);
  assert.deepEqual(first.hello.features, direct.hello.features);
  for (let attempt = 0; attempt < 50; attempt++) {
    const { result } = await direct.call('mcp.catalog', {});
    if (result.userApplication.appliedRevision === result.userRevision && result.pluginApplication.appliedRevision) break;
    await delay(50);
  }
  for (const [method, params] of [['mcp.catalog', {}], ['plugin.list', {}], ['sharing.absent', {}]]) {
    const { id: _local, ...local } = await direct.call(method, params);
    const { id: _remote, ...remote } = await first.call(method, params);
    assert.deepEqual(remote, local);
  }
  const folder = path.join(temp, 'remote-only');
  await mkdir(folder);
  for (const dir of ['vendor', 'target', '.hidden']) await mkdir(path.join(folder, dir));
  const file = path.join(folder, 'readme.txt');
  await writeFile(file, 'file');
  const directories = await first.call('workspace.files.directories', { path: folder });
  assert.deepEqual(directories.result.directories.map(dir => dir.name), ['target', 'vendor']);
  assert.ok((await first.call('workspace.files.directories', { path: file })).error);
  assert.deepEqual((await first.call('session.active', {})).result, []);
  const { result: session } = await direct.call('session.create', { cwd: temp });
  assert.equal((await first.call('session.attach', { sessionId: session.sessionId })).result.sessionId, session.sessionId);
  const {sessionId} = session;
  assert.ok(!(await readdir(staging)).includes('upload-abandoned'), 'startup cleaned a previous interrupted reception');
  const complete = await second.call('workspace.files.complete', {cwd: folder, query: 'readme'});
  assert.deepEqual(complete.result.candidates.map(({insertText, absolutePath, dir, image}) =>
    [insertText, absolutePath, dir, image]), [['readme.txt', await realpath(file), false, false]]);
  const upload = 'native.uploadSessionFile';
  assert.ok(second.hello.features.includes(upload));
  const bytes = Buffer.alloc(16 * 1024 * 1024);
  for (let offset = 0; offset < bytes.length; offset++) bytes[offset] = offset % 251;
  const header = {sessionId, name: '记录 1.bin', byteLength: bytes.length, image: false};
  const refused = await rpc(proxy);
  const tooLarge = await refused.call(upload, {...header, byteLength: bytes.length + 1});
  assert.equal(tooLarge.error.data.appError, 'attachment_too_large');
  refused.stream.destroy();
  const short = await rpc(proxy);
  const shortReady = short.receive(message => message.method === upload + '.ready');
  short.stream.write(JSON.stringify({jsonrpc: '2.0', id: 2, method: upload, params: header}) + '\n');
  await shortReady;
  short.stream.end(bytes.subarray(0, 100));
  short.stream.destroy();
  const binary = await rpc(proxy);
  const admitted = binary.receive(message => message.method === upload + '.ready');
  binary.stream.write(JSON.stringify({jsonrpc: '2.0', id: 2, method: upload, params: header}) + '\n');
  await admitted;
  binary.stream.write(bytes.subarray(0, 17));
  assert.equal((await second.call('session.active', {})).result[0].sessionId, sessionId);
  binary.stream.write(bytes.subarray(17));
  const receipt = await binary.receive(message => message.id === 2);
  assert.ok(receipt.result, JSON.stringify(receipt.error));
  assert.equal(receipt.result.bytes, bytes.length);
  assert.deepEqual(await readFile(receipt.result.path), bytes);
  binary.stream.destroy();
  const active = (await second.call('session.active', {})).result;
  assert.deepEqual(active.map(s => [s.sessionId, s.cwd, s.running]), [[session.sessionId, await realpath(temp), false]]);
  const notice = first.receive(message => message.method === 'session.event' && message.params.sessionId === session.sessionId);
  assert.ok((await direct.call('session.submit', { sessionId: session.sessionId, text: 'Hello' })).result);
  await notice;
  first.stream.destroy();
  const large = { key: 'x'.repeat(1024 * 1024) };
  const expected = await direct.call('sharing.absent', large);
  const actual = await second.call('sharing.absent', large);
  assert.deepEqual(actual.error, expected.error);
  const pendingClose = once(second.stream, 'close');
  await share('reset');
  await pendingClose;
  hold.stdin.end();
  const stale = await run([...clientArgs, '--', 'mcp', 'list']);
  assert.notEqual(stale.code, 0);
  assert.doesNotMatch(stale.err, /Sharing token: /, 'reconnect must try its saved credential');
  const current = await share('status');
  assert.notEqual(current.token, token);
  const repaired = await run([...clientArgs, '--pair', '--', 'mcp', 'list'], current.token + '\n');
  assert.equal(repaired.code, 0, repaired.err);
  const reconnected = await run([...clientArgs, '--', 'mcp', 'list']);
  assert.equal(reconnected.code, 0, reconnected.err);
  assert.doesNotMatch(reconnected.err, /Sharing token: /);
  const named = await run(['--home', a, '--target', current.name, '--cwd', folder, 'mcp', 'list']);
  assert.equal(named.code, 0, named.err);
  assert.equal(named.out, reconnected.out);
  const alternateAddress = address.replace('127.0.0.1', 'localhost');
  const alternate = await run(['--home', a, 'share', 'connect', alternateAddress, '--', 'mcp', 'list']);
  assert.equal(alternate.code, 0, alternate.err);
  assert.doesNotMatch(alternate.err, /Sharing token: /, 'a different route to the same authenticated instance can reconnect');
  assert.deepEqual((await share('disable')).addresses, []);
});

test('daemon stop reports the same cleanup result through direct and shared connections', { timeout: 90_000 }, async (t) => {
  const { temp, a, start, run } = await environment(t);
  for (const fail of [false, true]) {
    await t.test(fail ? 'remote stop preserves a delayed cleanup error' : 'remote stop waits for successful cleanup', async () => {
      const results = [];
      for (const shared of [false, true]) {
        const home = path.join(temp, 'stop-' + Number(fail) + Number(shared));
        const native = path.join(home, 'rust');
        const endpoint = windows ? String.raw`\\.\pipe\dim-test-${randomUUID()}` : path.join(native, 'daemon.sock');
        await mkdir(native, { recursive: true, mode: 0o700 });
        const owner = start(['--home', native, '--model-config', model, windows ? '--pipe' : '--unix', endpoint,
          '--no-ws', '--sharing-helper', helper], fixture, fail ? { DIM_TEST_FAIL_CLEANUP: '1' } : {});
        const ended = once(owner, 'close');
        let diagnostics = '';
        owner.stderr.on('data', chunk => { diagnostics += chunk; });
        const ready = createInterface({ input: owner.stdout })[Symbol.asyncIterator]();
        assert.equal((await ready.next()).done, false, diagnostics);
        const setup = await rpc(endpoint);
        assert.ok((await setup.call('session.create', { cwd: temp })).result);
        setup.stream.destroy();
        let args = ['--home', home], input = '';
        if (shared) {
          const enabled = await run(['--home', native, 'enable'], '', helper);
          assert.equal(enabled.code, 0, enabled.err);
          const { addresses, token } = JSON.parse(enabled.out);
          args = ['--home', a, 'share', 'connect', addresses[0], '--pair', '--'];
          input = token + '\n';
        }
        const directPipe = !shared && windows ? { DIMCODE_TERMINAL_PROXY: endpoint } : {};
        const result = await run([...args, 'daemon', 'stop'], input, cli, directPipe);
        assert.equal(result.code === 0, !fail, result.err);
        if (fail) assert.match(result.err, /shutdown_failed/);
        else assert.match(result.out, /Daemon stopped/);
        results.push(result);
        const [code] = await ended;
        assert.equal(code === 0, !fail);
      }
      const [direct, shared] = results;
      assert.equal(shared.code, direct.code);
      assert.equal(shared.out, direct.out);
      assert.equal(shared.err.replace('Sharing token: ', ''), direct.err);
    });
  }
});
