import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, statSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';

test('compiled Rust targets forward arguments, failures and reject missing or ambiguous artifacts', (t) => {
  const temporary = mkdtempSync(path.join(os.tmpdir(), 'dim test artifacts '));
  t.after(() => rmSync(temporary, { recursive: true, force: true }));
  const artifacts = path.join(temporary, 'artifacts.jsonl');
  const record = { reason: 'compiler-artifact', profile: { test: true }, target: { name: 'flow' }, manifest_path: path.join(temporary, 'Cargo.toml'), executable: process.execPath };
  const run = (...args) => spawnSync(process.execPath, [path.join(import.meta.dirname, 'run-rust-test.mjs'), artifacts, 'flow', ...args], { encoding: 'utf8' });
  writeFileSync(artifacts, [JSON.stringify({ reason: 'build-finished', success: true }), JSON.stringify(record)].join('\n')+'\n');
  const result = run('--eval', 'console.log(JSON.stringify({args:process.argv.slice(1),cwd:process.cwd()}))', '--', 'node_', '--ignored');
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), {args:['node_', '--ignored'],cwd:realpathSync(temporary)});
  assert.equal(run('--eval', 'process.exit(7)').status, 7);
  for (const records of [[], [{ ...record, profile: { test: false } }], [record, record]]) {
    writeFileSync(artifacts, records.map(value => JSON.stringify(value)).join('\n'));
    const rejected = run();
    assert.notEqual(rejected.status, 0);
    assert.match(rejected.stderr, /Expected one compiled test target named flow/);
  }
});

test('compiled workspace targets overlap in pairs and finish the queue after a failure', (t) => {
  const temporary = mkdtempSync(path.join(os.tmpdir(), 'dim parallel artifacts '));
  t.after(() => rmSync(temporary, { recursive: true, force: true }));
  const artifacts = path.join(temporary, 'artifacts.jsonl');
  const records = ['first', 'second', 'third'].map(name => {
    const directory = path.join(temporary, name);
    mkdirSync(directory);
    return { reason: 'compiler-artifact', profile: { test: true }, target: { name }, manifest_path: path.join(directory, 'Cargo.toml'), executable: process.execPath };
  });
  writeFileSync(artifacts, records.map(record => JSON.stringify(record)).join('\n'));
  const fixture = `
    const assert = require('node:assert/strict');
    const fs = require('node:fs');
    const path = require('node:path');
    const name = path.basename(process.cwd());
    const root = path.dirname(process.cwd());
    fs.writeFileSync('started', '');
    if (name === 'third') {
      assert(fs.existsSync(path.join(root, 'first/done')) || fs.existsSync(path.join(root, 'second/done')));
      fs.writeFileSync('done', '');
    } else {
      const other = name === 'first' ? 'second' : 'first';
      const until = Date.now() + 5000;
      const timer = setInterval(() => {
        if (fs.existsSync(path.join(root, other, 'started'))) {
          clearInterval(timer);
          fs.writeFileSync('done', '');
          process.exitCode = name === 'first' ? 7 : 0;
        } else if (Date.now() > until) {
          clearInterval(timer);
          process.exitCode = 9;
        }
      }, 10);
    }
  `;
  const result = spawnSync(process.execPath, [path.join(import.meta.dirname, 'run-rust-test.mjs'), artifacts, '--workspace', '--eval', fixture], { encoding: 'utf8', timeout: 10_000 });
  assert.equal(result.status, 7, result.stderr);
  for (const record of records) {
    assert(existsSync(path.join(path.dirname(record.manifest_path), 'done')), result.stdout);
    assert.match(result.stdout, new RegExp(`::group::${record.target.name}`));
  }
  writeFileSync(artifacts, '');
  const empty = spawnSync(process.execPath, [path.join(import.meta.dirname, 'run-rust-test.mjs'), artifacts, '--workspace'], { encoding: 'utf8' });
  assert.notEqual(empty.status, 0);
  assert.match(empty.stderr, /Expected compiled workspace test targets/);
});

test('developer entry runs without Node or plugin packages and preserves terminal invocation', { skip: process.platform === 'win32' }, (t) => {
  const temporary = mkdtempSync(path.join(os.tmpdir(), 'dim dev flow '));
  t.after(() => rmSync(temporary, { recursive: true, force: true }));
  const repo = path.join(temporary, 'public source');
  const workspace = path.join(temporary, 'caller workspace');
  const bin = path.join(temporary, 'tools');
  const home = path.join(temporary, 'isolated home');
  const calls = path.join(temporary, 'native-calls.jsonl');
  for (const directory of [repo, workspace, bin, path.join(repo, 'target/debug')]) mkdirSync(directory, { recursive: true });
  cpSync(path.resolve(import.meta.dirname, '../dev'), path.join(repo, 'dev'));
  writeFileSync(path.join(repo, '.env.dev'), "DIMCODE_HOME=/unused/configured-home\nDIMCODE_PROVIDER=configured-provider\nDIMCODE_MODEL=configured-model\n");
  writeFileSync(path.join(bin, 'cargo'), '#!/bin/bash\necho "build output"\nexit 0\n', { mode: 0o755 });
  writeFileSync(path.join(bin, 'node'), '#!/bin/bash\nexit 91\n', { mode: 0o755 });
  writeFileSync(path.join(repo, 'target/debug/dimcode'), `#!${process.execPath}
const fs = require('node:fs');
const args = process.argv.slice(2);
if (args.slice(-2).join(' ') === 'daemon stop') {
  fs.appendFileSync(process.env.TEST_CALLS, JSON.stringify({stop: args})+'\\n');
} else {
  console.log(JSON.stringify({args, cwd: process.cwd(), input: fs.readFileSync(0, 'utf8')}));
  process.exit(Number(process.env.TEST_NATIVE_EXIT || 0));
}
`, { mode: 0o755 });
  const model = path.join(workspace, 'model config.json');
  const env = { ...process.env, PATH: `${bin}:/usr/bin:/bin`, DIMCODE_HOME: home, DIMCODE_MODEL_CONFIG: '', DIMCODE_PROVIDER: 'openai', DIMCODE_MODEL: 'test-model', TEST_CALLS: calls };
  const run = (args, overrides = {}) => spawnSync(path.join(repo, 'dev'), args, { cwd: workspace, env: { ...env, ...overrides }, encoding: 'utf8', input: 'piped text\n' });
  const coldRepo = path.join(temporary, 'cold source');
  mkdirSync(coldRepo);
  cpSync(path.join(repo, 'dev'), path.join(coldRepo, 'dev'));
  const cold = spawnSync(path.join(coldRepo, 'dev'), ['stop'], { env, encoding: 'utf8' });
  assert.equal(cold.status, 0, cold.stderr);
  assert.equal(cold.stdout, '');
  assert.doesNotMatch(cold.stderr, /build output/);
  assert.equal(existsSync(home), false);
  const first = run(['--model-config', model, '--prompt', 'models']);
  assert.equal(first.status, 0, first.stderr);
  const forwarded = JSON.parse(first.stdout);
  assert.deepEqual(forwarded.args, ['--home', home, '--provider', 'openai', '--model', 'test-model', '--model-config', model, '--prompt', 'models']);
  assert.equal(forwarded.cwd, realpathSync(workspace));
  assert.equal(forwarded.input, 'piped text\n');
  assert.match(first.stderr, /build output/);
  assert.equal(statSync(home).mode & 0o077, 0);
  assert.equal(existsSync(path.join(repo, '.dev')), false);
  const account = run(['login', 'dimcode-api-oauth']);
  assert.equal(account.status, 0, account.stderr);
  assert.deepEqual(JSON.parse(account.stdout).args, ['--home', home, 'login', 'dimcode-api-oauth']);
  const resumed = run(['--model-config', model, '--resume', 'session with spaces'], { TEST_NATIVE_EXIT: '23' });
  assert.equal(resumed.status, 23, resumed.stderr);
  assert.deepEqual(JSON.parse(resumed.stdout).args, ['--home', home, '--model-config', model, '--resume', 'session with spaces']);
  const migrationHome = path.join(temporary, 'migration target');
  for (const args of [['migration'], ['migration', 'inspect'], ['migration', 'cancel', 'task'], ['migration', 'retry', 'task']]) {
    const migration = run(args, { DIMCODE_HOME: migrationHome, DIMCODE_MODEL_CONFIG: model });
    assert.equal(migration.status, 0, migration.stderr);
    assert.deepEqual(JSON.parse(migration.stdout).args, args);
    assert.equal(existsSync(migrationHome), false);
  }
  const scopedMigration = run(['migration', '--home', migrationHome]);
  assert.equal(scopedMigration.status, 0, scopedMigration.stderr);
  assert.deepEqual(JSON.parse(scopedMigration.stdout).args, ['--home', migrationHome, 'migration', '--home', migrationHome]);
  const sourceMigration = run(['migration', '--source', '/legacy/v2']);
  assert.equal(sourceMigration.status, 0, sourceMigration.stderr);
  assert.deepEqual(JSON.parse(sourceMigration.stdout).args, ['migration', '--source', '/legacy/v2']);
  const stopped = run(['stop']);
  assert.equal(stopped.status, 0, stopped.stderr);
  assert.equal(stopped.stdout, '');
  const stops = readFileSync(calls, 'utf8').trim().split('\n').map((line) => JSON.parse(line));
  assert.equal(stops.length, 4);
  assert(stops.every((call) => JSON.stringify(call.stop) === JSON.stringify(['--home', home, 'daemon', 'stop'])));
});

test('development home starts without writing an isolation policy', { skip: process.platform === 'win32' }, (t) => {
  const temporary = mkdtempSync(path.join(os.tmpdir(), 'dim dev isolation '));
  t.after(() => rmSync(temporary, { recursive: true, force: true }));
  const repo = path.join(temporary, 'public source');
  const bin = path.join(temporary, 'tools');
  const home = path.join(temporary, 'isolated home');
  for (const directory of [repo, bin, path.join(repo, 'target/debug')]) mkdirSync(directory, { recursive: true });
  cpSync(path.resolve(import.meta.dirname, '../dev'), path.join(repo, 'dev'));
  writeFileSync(path.join(bin, 'cargo'), '#!/bin/bash\nexit 0\n', { mode: 0o755 });
  writeFileSync(path.join(repo, 'target/debug/dimcode'), '#!/bin/bash\nexit 0\n', { mode: 0o755 });
  const env = { ...process.env, PATH: `${bin}:/usr/bin:/bin`, DIMCODE_HOME: home, DIMCODE_MODEL_CONFIG: '', DIMCODE_PROVIDER: '', DIMCODE_MODEL: '' };
  const run = (...args) => spawnSync(path.join(repo, 'dev'), ['--prompt', 'hello', ...args], { env, encoding: 'utf8', input: '' });
  const created = run();
  assert.equal(created.status, 0, created.stderr);
  assert.equal(existsSync(path.join(home, 'host-isolation.json')), false);
  const configured = path.join(temporary, 'model.json');
  writeFileSync(configured, '{}');
  const configured_home = path.join(temporary, 'configured home');
  const configured_run = spawnSync(path.join(repo, 'dev'), ['--model-config', configured, '--prompt', 'hello'], {
    env: { ...env, DIMCODE_HOME: configured_home },
    encoding: 'utf8',
    input: '',
  });
  assert.equal(configured_run.status, 0, configured_run.stderr);
  assert.equal(existsSync(path.join(configured_home, 'rust/host-isolation.json')), false);
});

/** A source checkout with two code plugins, one skills-only plugin and stubs for cargo, node and npm. */
function pluginCheckout(t, prefix) {
  const temporary = realpathSync(mkdtempSync(path.join(os.tmpdir(), prefix)));
  t.after(() => rmSync(temporary, { recursive: true, force: true }));
  const repo = path.join(temporary, 'public source');
  const bin = path.join(temporary, 'tools');
  const home = path.join(temporary, 'isolated home');
  const log = path.join(temporary, 'npm.log');
  for (const relative of ['scripts', 'target/debug', 'runtime/plugins/goal', 'runtime/plugins/session-title', 'runtime/plugins/workflow']) {
    mkdirSync(path.join(repo, relative), { recursive: true });
  }
  mkdirSync(bin);
  cpSync(path.resolve(import.meta.dirname, '../dev'), path.join(repo, 'dev'));
  cpSync(path.join(import.meta.dirname, 'stage-plugins.mjs'), path.join(repo, 'scripts/stage-plugins.mjs'));
  writeFileSync(path.join(repo, 'runtime/default-plugins.json'),
    `${JSON.stringify({ plugins: { goal: 'plugins/goal', 'session-title': 'plugins/session-title', workflow: 'plugins/workflow' } })}\n`);
  for (const name of ['goal', 'session-title']) {
    writeFileSync(path.join(repo, `runtime/plugins/${name}/package.json`), `${JSON.stringify({ name: `@dim/${name}`, private: true })}\n`);
    writeFileSync(path.join(repo, `runtime/plugins/${name}/package-lock.json`), `${JSON.stringify({ name: `@dim/${name}`, lockfileVersion: 3 })}\n`);
  }
  for (const name of ['goal', 'session-title', 'workflow']) {
    writeFileSync(path.join(repo, `runtime/plugins/${name}/index.ts`), `export const name = '${name}';\n`);
  }
  writeFileSync(path.join(bin, 'cargo'), '#!/bin/bash\nexit 0\n', { mode: 0o755 });
  writeFileSync(path.join(bin, 'node'), `#!/bin/bash\nexec "${process.execPath}" "$@"\n`, { mode: 0o755 });
  const npm = (status) => writeFileSync(path.join(bin, 'npm'),
    `#!/bin/bash\nprintf '%s\\n' "$PWD" >> "${log}"\nmkdir -p "$PWD/node_modules"\nexit ${status}\n`, { mode: 0o755 });
  npm(0);
  writeFileSync(path.join(repo, 'target/debug/dimcode'), `#!${process.execPath}
const args = process.argv.slice(2);
if (args.slice(-2).join(' ') === 'daemon stop') process.exit(0);
console.log(JSON.stringify({ defaultPlugins: process.env.DIMCODE_DEFAULT_PLUGINS ?? null }));
`, { mode: 0o755 });
  const env = { ...process.env, PATH: `${bin}:/usr/bin:/bin`, DIMCODE_HOME: home, DIMCODE_MODEL_CONFIG: '', DIMCODE_PROVIDER: '', DIMCODE_MODEL: '' };
  delete env.DIMCODE_DEFAULT_PLUGINS;
  const run = (overrides = {}) => spawnSync(path.join(repo, 'dev'), ['--prompt', 'hello'], { cwd: temporary, env: { ...env, ...overrides }, encoding: 'utf8', input: '' });
  const installs = () => existsSync(log) ? readFileSync(log, 'utf8').trim().split('\n').filter(Boolean) : [];
  return { temporary, repo, bin, home, log, staged: path.join(repo, 'staging/dev-plugins'), run, installs, npm };
}

/** The plugin a logged npm ci ran for; the temporary directory carries the pid. */
const installName = (directory) => {
  const match = /^(.*)\.partial(?:\.\d+)?$/.exec(path.basename(directory));
  return match ? match[1] : path.basename(directory);
};

/** Temporary directories a stage holds for one plugin. */
const leftovers = (output, relative, kind) => {
  const parent = path.dirname(path.join(output, relative));
  const name = path.basename(relative).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  if (!existsSync(parent)) return [];
  const pattern = new RegExp(`^${name}\\.${kind}(?:\\.\\d+)?$`);
  return readdirSync(parent).filter((entry) => pattern.test(entry));
};

test('a run stages the default plugins and updates them only when needed', { skip: process.platform === 'win32' }, (t) => {
  const checkout = pluginCheckout(t, 'dim dev plugins ');
  const first = checkout.run();
  assert.equal(first.status, 0, first.stderr);
  assert.equal(JSON.parse(first.stdout).defaultPlugins, path.join(checkout.staged, 'default-plugins.json'));
  for (const file of ['default-plugins.json', 'plugins/goal/index.ts', 'plugins/session-title/index.ts', 'plugins/workflow/index.ts']) {
    assert(existsSync(path.join(checkout.staged, file)), file);
  }
  assert.deepEqual(checkout.installs().map(installName).sort(), ['goal', 'session-title']);
  assert.match(first.stderr, /Updated 3 default plugins \(npm ci: goal, session-title\)\./);
  const second = checkout.run();
  assert.equal(second.status, 0, second.stderr);
  assert.equal(checkout.installs().length, 2);
  assert.match(second.stderr, /Updated 3 default plugins \(npm ci: none\)\./);
  writeFileSync(path.join(checkout.repo, 'runtime/plugins/goal/index.ts'), "export const name = 'goal edited';\n");
  const edited = checkout.run();
  assert.equal(edited.status, 0, edited.stderr);
  assert.equal(readFileSync(path.join(checkout.staged, 'plugins/goal/index.ts'), 'utf8'), "export const name = 'goal edited';\n");
  assert.equal(checkout.installs().length, 2);
  writeFileSync(path.join(checkout.repo, 'runtime/plugins/goal/package-lock.json'), `${JSON.stringify({ name: '@dim/goal', lockfileVersion: 3, revision: 2 })}\n`);
  const locked = checkout.run();
  assert.equal(locked.status, 0, locked.stderr);
  assert.equal(checkout.installs().length, 3);
  assert.equal(installName(checkout.installs().at(-1)), 'goal');
  assert.match(locked.stderr, /Updated 3 default plugins \(npm ci: goal\)\./);
});

test('a developer-selected default plugin file is used without staging', { skip: process.platform === 'win32' }, (t) => {
  const checkout = pluginCheckout(t, 'dim dev configured ');
  const configured = path.join(checkout.temporary, 'configured/default-plugins.json');
  const result = checkout.run({ DIMCODE_DEFAULT_PLUGINS: configured });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout).defaultPlugins, configured);
  assert.equal(existsSync(path.join(checkout.repo, 'staging')), false);
  assert.deepEqual(checkout.installs(), []);
});

test('a missing npm or a failed update runs without the default plugins', { skip: process.platform === 'win32' }, (t) => {
  const checkout = pluginCheckout(t, 'dim dev unavailable ');
  const staged = checkout.run();
  assert.equal(staged.status, 0, staged.stderr);
  writeFileSync(path.join(checkout.staged, 'plugins/goal/marker'), 'kept');
  writeFileSync(path.join(checkout.repo, 'runtime/plugins/goal/package-lock.json'), `${JSON.stringify({ name: '@dim/goal', lockfileVersion: 3, revision: 2 })}\n`);
  checkout.npm(1);
  const failed = checkout.run();
  assert.equal(failed.status, 0, failed.stderr);
  assert.equal(JSON.parse(failed.stdout).defaultPlugins, null);
  assert.equal((failed.stderr.match(/The default plugins were not staged; running without them\./g) ?? []).length, 1);
  assert.match(failed.stderr, /Cannot stage goal/);
  assert.equal(readFileSync(path.join(checkout.staged, 'plugins/goal/marker'), 'utf8'), 'kept');
  assert.deepEqual(leftovers(checkout.staged, 'plugins/goal', 'partial'), []);
  const withoutNpm = path.join(checkout.temporary, 'without npm');
  mkdirSync(withoutNpm);
  for (const name of ['cargo', 'node']) cpSync(path.join(checkout.bin, name), path.join(withoutNpm, name));
  const missing = checkout.run({ PATH: `${withoutNpm}:/usr/bin:/bin` });
  assert.equal(missing.status, 0, missing.stderr);
  assert.equal(JSON.parse(missing.stdout).defaultPlugins, null);
  assert.equal((missing.stderr.match(/Node or npm is missing; running without the default plugins\./g) ?? []).length, 1);
});
