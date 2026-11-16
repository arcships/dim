import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const script = fileURLToPath(new URL('./publish-npm.mjs', import.meta.url));
/** The checker's waits are injectable, so a run fits the fixture's ten-second budget. */
const settle = { DIM_REGISTRY_SETTLE_MS: '2000', DIM_REGISTRY_POLL_MS: '25' };
const sourceCommit = 'a'.repeat(40);
const platforms = [
  ['darwin-arm64', 'darwin', 'arm64'], ['darwin-x64', 'darwin', 'x64'],
  ['linux-arm64', 'linux', 'arm64'], ['linux-x64', 'linux', 'x64'],
  ['windows-arm64', 'win32', 'arm64'], ['windows-x64', 'win32', 'x64'],
];

function fixture(t, version = '1.2.3', shipped = platforms) {
  const root = mkdtempSync(path.join(os.tmpdir(), 'dim-npm-release-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const alpha = version.includes('-alpha.');
  const wrapper = alpha ? 'dimcode-alpha' : 'dimcode';
  const names = [...shipped.map(([platform]) => `dimcode-${platform}`), 'dimcode-web', wrapper];
  const manifest = { sourceCommit, version, account: true, npmPackages: [] };
  for (const name of names) {
    const directory = path.join(root, name);
    mkdirSync(path.join(directory, 'package'), { recursive: true });
    const metadata = {
      name, version, repository: { type: 'git', url: 'git+https://github.com/arcships/dim.git' },
    };
    if (name === wrapper) {
      metadata.optionalDependencies = Object.fromEntries(names.slice(0, -1).map((name) => [name, version]));
      metadata.bin = alpha ? { dima: 'bin/dim.mjs' } : { dim: 'bin/dim.mjs', dimcode: 'bin/dim.mjs' };
    } else if (name === 'dimcode-web') {
    } else {
      const [, os, cpu] = platforms.find(([platform]) => name === `dimcode-${platform}`);
      metadata.os = [os];
      metadata.cpu = [cpu];
    }
    writeFileSync(path.join(directory, 'package/package.json'), JSON.stringify(metadata));
    if (name !== wrapper) writeFileSync(path.join(directory, 'package/release-manifest.json'), JSON.stringify({
      version, sourceCommit, platform: name.slice('dimcode-'.length), account: true,
    }));
    const file = `${name}-${version}.tgz`;
    const packed = spawnSync('tar', ['-czf', path.join(root, file), '-C', directory, 'package']);
    assert.equal(packed.status, 0);
    manifest.npmPackages.push({ name, file,
      sha256: createHash('sha256').update(readFileSync(path.join(root, file))).digest('hex') });
  }
  const bin = path.join(root, 'bin');
  mkdirSync(bin);
  writeFileSync(path.join(bin, 'npm'), `#!/bin/sh
if [ "$1" = view ]; then
  if [ "$2" = dimcode ] && [ "$3" = versions ]; then printf '%s' "$REGISTRY_VERSIONS"; exit 0; fi
  if [ "$REGISTRY_MODE" = newer ] && [ "$2" = dimcode-web@alpha ]; then printf '"1.2.3-alpha.10"'; exit 0; fi
  if [ "$REGISTRY_MODE" = newer-latest ] && [ "$2" = dimcode-alpha@latest ]; then printf '"1.2.3-alpha.10"'; exit 0; fi
  if [ "$REGISTRY_MODE" = fail ]; then printf '{"error":{"code":"E401"}}'; exit 1; fi
  if [ "$REGISTRY_MODE" = mismatch ]; then printf '{"integrity":"sha512-wrong"}'; exit 0; fi
  if [ "$REGISTRY_MODE" = existing ] && [ "$2" = dimcode-darwin-arm64@1.2.3 ]; then
    printf '{"integrity":"%s"}' "$EXISTING_INTEGRITY"; exit 0
  fi
  printf '{"error":{"code":"E404"}}'; exit 1
fi
printf "%s\\n" "$*" >> "$PUBLISH_LOG"
`, { mode: 0o755 });
  const log = path.join(root, 'published.log');
  writeFileSync(log, '');
  const release = { tag_name: `v${version}`, draft: false, published_at: '2026-10-14T00:00:00Z',
    assets: [...manifest.npmPackages.map(({ file }) => ({ name: file })), { name: 'release-manifest.json' }] };
  const releasePath = path.join(root, 'github-release.json');
  function run({ publish = false, checkRelease = false, local = false, commit = sourceCommit, registry = 'missing', versions = [] } = {}) {
    writeFileSync(path.join(root, 'release-manifest.json'), JSON.stringify(manifest));
    writeFileSync(releasePath, JSON.stringify(release));
    return spawnSync(process.execPath, [script, root, version, commit, ...(local ? ['--publish-local'] : publish ? ['--publish', releasePath] : checkRelease ? ['--check-release', releasePath] : [])], {
      encoding: 'utf8', env: { ...process.env, PATH: `${bin}${path.delimiter}${process.env.PATH}`, PUBLISH_LOG: log,
        REGISTRY_MODE: registry, REGISTRY_VERSIONS: JSON.stringify(versions), EXISTING_INTEGRITY: 'sha512-' + createHash('sha512').update(readFileSync(path.join(root, manifest.npmPackages[0].file))).digest('base64') },
    });
  }
  return { root, names, manifest, release, log, run };
}

test('validates Release and registry state without publishing', (t) => {
  const { release, manifest, log, run } = fixture(t, '1.2.3-alpha.2');
  assert.equal(run({ checkRelease: true }).status, 0);
  assert.notEqual(run({ publish: true, registry: 'newer' }).status, 0);
  assert.notEqual(run({ publish: true, registry: 'newer-latest' }).status, 0);
  for (const registry of ['fail', 'mismatch']) assert.notEqual(run({ checkRelease: true, registry }).status, 0);
  assert.notEqual(run({ checkRelease: true, versions: ['1.2.2-beta.0'] }).status, 0);
  manifest.account = false;
  assert.notEqual(run({ publish: true }).status, 0);
  manifest.account = true;
  release.draft = true;
  assert.notEqual(run({ checkRelease: true }).status, 0);
  assert.equal(readFileSync(log, 'utf8'), '');
});

test('rejects draft, unpublished, mismatched and incomplete Releases before registry writes', (t) => {
  const fixtureRelease = fixture(t);
  const { release, log, run } = fixtureRelease;
  release.draft = true;
  assert.notEqual(run({ publish: true }).status, 0);
  release.draft = false;
  release.published_at = null;
  assert.notEqual(run({ publish: true }).status, 0);
  release.published_at = '2026-10-14T00:00:00Z';
  release.tag_name = 'v1.2.4';
  assert.notEqual(run({ publish: true }).status, 0);
  release.tag_name = 'v1.2.3';
  release.assets.pop();
  assert.notEqual(run({ publish: true }).status, 0);
  assert.equal(readFileSync(log, 'utf8'), '');
});

test('validates without publishing and publishes the wrapper after every platform', (t) => {
  const release = fixture(t);
  assert.equal(release.run().status, 0);
  assert.equal(readFileSync(release.log, 'utf8'), '');
  const published = release.run({ publish: true });
  assert.equal(published.status, 0, published.stderr);
  const calls = readFileSync(release.log, 'utf8').trim().split('\n');
  assert.equal(calls.length, 8);
  for (const [index, name] of release.names.entries()) {
    assert.equal(calls[index], `publish ${path.join(release.root, `${name}-1.2.3.tgz`)} --access public --tag latest --ignore-scripts --registry https://registry.npmjs.org`);
  }
});

test('only an alpha ships a subset of platforms, with a wrapper referencing exactly them', (t) => {
  const darwin = platforms.slice(0, 1);
  const alpha = fixture(t, '1.2.3-alpha.0', darwin);
  const published = alpha.run({ local: true, versions: ['1.2.3-beta.0'] });
  assert.equal(published.status, 0, published.stderr);
  assert.deepEqual(readFileSync(alpha.log, 'utf8').trim().split('\n').map((call) => path.basename(call.split(' ')[1])),
    ['dimcode-darwin-arm64-1.2.3-alpha.0.tgz', 'dimcode-web-1.2.3-alpha.0.tgz', 'dimcode-alpha-1.2.3-alpha.0.tgz']);
  assert.notEqual(alpha.run({ checkRelease: true }).status, 0);
  const stable = fixture(t, '1.2.3', darwin);
  assert.notEqual(stable.run().status, 0);
  assert.equal(readFileSync(stable.log, 'utf8'), '');
});

test('rejects a corrupted wrapper before any platform is published', (t) => {
  const release = fixture(t);
  writeFileSync(path.join(release.root, 'dimcode-1.2.3.tgz'), 'corrupt');
  assert.notEqual(release.run({ publish: true }).status, 0);
  assert.equal(readFileSync(release.log, 'utf8'), '');
});

test('rejects wrong source commits, missing platforms and mismatched packed versions', (t) => {
  const release = fixture(t);
  assert.notEqual(release.run({ publish: true, commit: 'b'.repeat(40) }).status, 0);
  const platform = release.manifest.npmPackages.shift();
  assert.notEqual(release.run({ publish: true }).status, 0);
  release.manifest.npmPackages.unshift(platform);
  const packageDirectory = path.join(release.root, 'dimcode');
  const metadataPath = path.join(packageDirectory, 'package/package.json');
  const metadata = JSON.parse(readFileSync(metadataPath, 'utf8'));
  metadata.version = '1.2.4';
  writeFileSync(metadataPath, JSON.stringify(metadata));
  const wrapper = release.manifest.npmPackages.at(-1);
  const archive = path.join(release.root, wrapper.file);
  assert.equal(spawnSync('tar', ['-czf', archive, '-C', packageDirectory, 'package']).status, 0);
  wrapper.sha256 = createHash('sha256').update(readFileSync(archive)).digest('hex');
  assert.notEqual(release.run({ publish: true }).status, 0);
  assert.equal(readFileSync(release.log, 'utf8'), '');
});

test('an alpha wrapper installs only dima, so it never replaces dim', (t) => {
  const release = fixture(t, '1.2.3-alpha.1');
  assert.equal(release.manifest.npmPackages.at(-1).name, 'dimcode-alpha');
  assert.equal(release.run().status, 0);
  const packageDirectory = path.join(release.root, 'dimcode-alpha');
  const metadataPath = path.join(packageDirectory, 'package/package.json');
  const metadata = JSON.parse(readFileSync(metadataPath, 'utf8'));
  metadata.bin.dim = metadata.bin.dima;
  writeFileSync(metadataPath, JSON.stringify(metadata));
  const wrapper = release.manifest.npmPackages.at(-1);
  const archive = path.join(release.root, wrapper.file);
  assert.equal(spawnSync('tar', ['-czf', archive, '-C', packageDirectory, 'package']).status, 0);
  wrapper.sha256 = createHash('sha256').update(readFileSync(archive)).digest('hex');
  assert.notEqual(release.run().status, 0);
});

test('each channel publishes to its own dist-tag', (t) => {
  for (const [version, tag] of [['1.2.3-beta.1', 'beta'], ['1.2.3-alpha.1', 'alpha']]) {
    const release = fixture(t, version);
    const published = release.run({ publish: true });
    assert.equal(published.status, 0, published.stderr);
    const calls = readFileSync(release.log, 'utf8').trim().split('\n');
    assert.equal(calls.length, 8);
    for (const call of calls) assert(call.endsWith(`--tag ${tag} --ignore-scripts --registry https://registry.npmjs.org`));
  }
  assert.notEqual(fixture(t, '1.2.3-rc.1').run().status, 0);
});

test('publishes only alphas from a local build', (t) => {
  const beta = fixture(t, '1.2.3-beta.1');
  assert.notEqual(beta.run({ local: true }).status, 0);
  assert.equal(readFileSync(beta.log, 'utf8'), '');
  const alpha = fixture(t, '1.2.3-alpha.1');
  const published = alpha.run({ local: true });
  assert.equal(published.status, 0, published.stderr);
  const calls = readFileSync(alpha.log, 'utf8').trim().split('\n');
  assert.equal(calls.length, 8);
  for (const call of calls) assert(call.includes('--tag alpha'));
});

test('rejects an alpha that would outrank the newest beta before registry writes', (t) => {
  const release = fixture(t, '1.2.3-alpha.1');
  for (const versions of [['1.2.2', '1.2.2-beta.4'], ['1.2.3-beta.0', '2.0.0']]) {
    const allowed = versions.includes('1.2.3-beta.0');
    assert.equal(release.run({ publish: true, versions }).status === 0, allowed);
    if (!allowed) assert.equal(readFileSync(release.log, 'utf8'), '');
  }
});

 test('retries matching published packages and stops before writes on mismatches or registry errors', (t) => {
  const release = fixture(t);
  const resumed = release.run({publish:true,registry:'existing'});
  assert.equal(resumed.status,0,resumed.stderr);
  const calls = readFileSync(release.log,'utf8').trim().split('\n');
  assert.equal(calls.length,7);
  assert(calls[0].includes('dimcode-darwin-x64-1.2.3.tgz'));
  writeFileSync(release.log,'');
  assert.notEqual(release.run({publish:true,registry:'mismatch'}).status,0);
  assert.equal(readFileSync(release.log,'utf8'),'');
  assert.notEqual(release.run({publish:true,registry:'fail'}).status,0);
  assert.equal(readFileSync(release.log,'utf8'),'');
});

function nightlyRegistry(t, shipped = platforms) {
  const { root, manifest, names } = fixture(t, '1.2.3-alpha.10', shipped);
  const receipt = path.join(root, 'npm-publication.json');
  writeFileSync(receipt, JSON.stringify(manifest));
  const remote = path.join(root, 'registry.json');
  const initialTags = Object.fromEntries(['dimcode', ...names].map(name => [name, name === 'dimcode-alpha'
    ? { latest: '1.2.3-alpha.9', alpha: manifest.version, canary: '1.2.3-alpha.8' }
    : { latest: '1.2.2', beta: '1.2.3-beta.1', alpha: manifest.version }]));
  writeFileSync(remote, JSON.stringify({ tags: initialTags, installs: [], syncs: [] }));
  const preload = path.join(root, 'registry.mjs');
  writeFileSync(preload, `
    import assert from 'node:assert/strict';
    import { readFileSync, writeFileSync } from 'node:fs';
    import { createHash } from 'node:crypto';
    import path from 'node:path';
    const failure = process.env.FAILURE;
    const tarballMisses = Number(process.env.TARBALL_404 ?? 0);
    const transient = Number(process.env.TRANSIENT ?? 0);
    const bodyResets = Number(process.env.BODY_RESET ?? 0);
    const root = ${JSON.stringify(root)};
    const lagFile = ${JSON.stringify(path.join(root, 'tag-lag'))};
    const lagLeft = () => { try { return Number(readFileSync(lagFile, 'utf8').trim()); } catch { return 0; } };
    const spendLag = () => writeFileSync(lagFile, String(lagLeft() - 1));
    let transientLeft = transient;
    let bodyResetsLeft = bodyResets;
    const remote = ${JSON.stringify(remote)};
    const names = ${JSON.stringify(names)};
    const version = ${JSON.stringify(manifest.version)};
    let wrapperReads = 0;
    let tarballReads = 0;
    globalThis.fetch = async (url, options = {}) => {
      const { hostname, pathname } = new URL(url);
      assert(['registry.npmjs.org', 'registry.npmmirror.com'].includes(hostname));
      const state = JSON.parse(readFileSync(remote));
      if (options.method === 'PUT') {
        assert.equal(hostname, 'registry.npmmirror.com');
        state.syncs.push(pathname);
        writeFileSync(remote, JSON.stringify(state));
        return Response.json({ ok: failure !== 'sync' });
      }
      if (pathname.startsWith('/-/package/')) {
        const name = pathname.split('/')[3];
        assert(name === 'dimcode' || names.includes(name));
        if (failure === 'tag-auth') return new Response('', { status: 401 });
        if (failure === 'tag-server') return new Response('', { status: 503 });
        if (failure === 'tag-network') throw new Error('network failure');
        if (!state.tags[name]) return new Response('', { status: 404 });
        if (failure === 'race' && name === 'dimcode-alpha' && ++wrapperReads === 3) {
          state.tags[name].latest = '1.2.3-alpha.11';
          writeFileSync(remote, JSON.stringify(state));
        }
        const tags = { ...state.tags[name] };
        if (name === 'dimcode-alpha' && lagLeft() > 0) {
          spendLag();
          tags.latest = '1.2.3-alpha.9';
        }
        if (failure === 'tags' && name === 'dimcode-web') tags.beta = '2.0.0-beta.1';
        return Response.json(tags);
      }
      const [, name, ref] = pathname.split('/');
      assert(names.includes(name));
      if (ref === '-' && transientLeft > 0) { transientLeft -= 1; throw new Error('connection reset'); }
      if (failure === 'auth') return new Response('', { status: 401 });
      if (failure === 'network') throw new Error('network failure');
      if (failure === 'server') return new Response('', { status: 503 });
      if (failure === 'missing' && name === 'dimcode-windows-x64') return new Response('', { status: 404 });
      if (ref === '-') {
        if (tarballReads < tarballMisses) { tarballReads += 1; return new Response('', { status: 404 }); }
        return new Response(readFileSync(path.join(root, name + '-' + version + '.tgz')));
      }
      if (ref !== version && !['latest', 'alpha'].includes(ref)) return new Response('', { status: 404 });
      const metadata = JSON.parse(readFileSync(path.join(root, name, 'package/package.json')));
      if (ref === 'latest') metadata.version = failure === 'stale' ? '1.2.3-alpha.9' : state.tags[name].latest;
      if (name === 'dimcode-alpha') {
        if (failure === 'bin') metadata.bin = { dim: 'bin/dim.mjs' };
        if (failure === 'dependencies') metadata.optionalDependencies['dimcode-web'] = '1.2.3-alpha.9';
      } else {
        const bytes = readFileSync(path.join(root, name + '-' + version + '.tgz'));
        metadata.dist = { tarball: 'https://registry.npmjs.org/' + name + '/-/' + name + '-' + version + '.tgz',
          integrity: 'sha512-' + createHash('sha512').update(bytes).digest('base64') };
        if (failure === 'integrity') metadata.dist.integrity = 'sha512-corrupt';
      }
      // Headers arrive, then the body transfer fails: fetch() has already
      // resolved, so only a boundary that consumes the body can retry this.
      if (bodyResetsLeft > 0) {
        bodyResetsLeft -= 1;
        return new Response(new ReadableStream({
          start(controller) { controller.error(new TypeError('terminated')); },
        }), { status: 200, headers: { 'content-type': 'application/json' } });
      }
      return Response.json(metadata);
    };
  `);
  writeFileSync(path.join(root, 'bin/npm'), `#!${process.execPath}
    const { mkdirSync, readFileSync, writeFileSync } = require('node:fs');
    const path = require('node:path');
    const args = process.argv.slice(2);
    const remote = ${JSON.stringify(remote)};
    const lagFile = ${JSON.stringify(path.join(root, 'tag-lag'))};
    const state = JSON.parse(readFileSync(remote));
    if (args[0] === 'dist-tag') {
      if (args[1] !== 'add' || args[2] !== 'dimcode-alpha@1.2.3-alpha.10' || !['latest', 'alpha'].includes(args[3])) process.exit(1);
      state.tags['dimcode-alpha'][args[3]] = '1.2.3-alpha.10';
      writeFileSync(lagFile, process.env.TAG_LAG ?? '0');
    } else if (args[0] === 'install') {
      const prefix = args[args.indexOf('--prefix') + 1];
      const cache = args[args.indexOf('--cache') + 1];
      const registry = args[args.indexOf('--registry') + 1];
      const spec = args.at(-1);
      const version = process.env.FAILURE === 'old-install' ? '1.2.3-alpha.9' : '1.2.3-alpha.10';
      const directory = path.join(prefix, 'lib/node_modules/dimcode-alpha');
      mkdirSync(directory, { recursive: true });
      mkdirSync(path.join(prefix, 'bin'), { recursive: true });
      const metadata = JSON.parse(readFileSync(${JSON.stringify(path.join(root, 'dimcode-alpha/package/package.json'))}));
      metadata.version = version;
      writeFileSync(path.join(directory, 'package.json'), JSON.stringify(metadata));
      writeFileSync(path.join(prefix, 'bin/dima'), '#!/bin/sh\\necho dimcode ' + version + '\\n', { mode: 0o755 });
      state.installs.push({ spec, prefix, cache, registry });
    } else process.exit(1);
    writeFileSync(remote, JSON.stringify(state));
  `, { mode: 0o755 });
  const check = fileURLToPath(new URL('./check-alpha-registry.mjs', import.meta.url));
  return { receipt, remote, initialTags,
    run(mode, flag = '--dry-run', failure = '', { tarballMisses = 0, tagLag = 0, transient = 0, bodyResets = 0 } = {}) {
      return spawnSync(process.execPath, ['--import', preload, check, receipt, mode, flag], {
        encoding: 'utf8', timeout: 10_000,
        env: { ...process.env, PATH: `${path.join(root, 'bin')}${path.delimiter}${process.env.PATH}`, FAILURE: failure,
          TARBALL_404: String(tarballMisses), TAG_LAG: String(tagLag), TRANSIENT: String(transient),
          BODY_RESET: String(bodyResets), ...settle },
      });
    },
    read() { return JSON.parse(readFileSync(remote)); },
  };
}

test('reuses a complete nightly, repairs only its wrapper tags and verifies both installations on both registries', (t) => {
  const registry = nightlyRegistry(t);
  writeFileSync(registry.receipt, JSON.stringify({ sourceCommit }));
  for (const [mode, flag] of [['source', '--dry-run'], ['promote', '--dry-run']]) {
    const result = registry.run(mode, flag);
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(registry.read().tags, registry.initialTags);
  }
  assert.equal(JSON.parse(readFileSync(registry.receipt)).version, '1.2.3-alpha.10');
  const promoted = registry.run('promote', '--apply');
  assert.equal(promoted.status, 0, promoted.stderr);
  const expected = structuredClone(registry.initialTags);
  expected['dimcode-alpha'].latest = '1.2.3-alpha.10';
  assert.deepEqual(registry.read().tags, expected);
  for (const mode of ['npm', 'mirror']) {
    const result = registry.run(mode);
    assert.equal(result.status, 0, result.stderr);
  }
  const { installs, syncs } = registry.read();
  assert.deepEqual(syncs, []);
  assert.deepEqual(installs.map(({ spec }) => spec), ['dimcode-alpha', 'dimcode-alpha@alpha', 'dimcode-alpha', 'dimcode-alpha@alpha']);
  assert.equal(new Set(installs.map(({ cache }) => cache)).size, 4);
  for (const { prefix, cache } of installs) {
    assert.equal(existsSync(prefix), false);
    assert.equal(existsSync(cache), false);
  }
  const synced = registry.run('mirror', '--apply');
  assert.equal(synced.status, 0, synced.stderr);
  assert.equal(registry.read().syncs.length, 8);
  assert.deepEqual(registry.read().tags, expected);
  const lostAlpha = registry.read();
  lostAlpha.tags['dimcode-alpha'].alpha = '1.2.3-alpha.9';
  writeFileSync(registry.remote, JSON.stringify(lostAlpha));
  assert.equal(registry.run('source').status, 0);
  const repairedAlpha = registry.run('promote', '--apply');
  assert.equal(repairedAlpha.status, 0, repairedAlpha.stderr);
  assert.deepEqual(registry.read().tags, expected);
});

test('fails closed on registry errors, incomplete builds, concurrent promotion and stale installations', (t) => {
  const registry = nightlyRegistry(t);
  assert.equal(registry.run('snapshot').status, 0);
  for (const failure of ['tag-auth', 'tag-network', 'tag-server', 'auth', 'network', 'server', 'integrity', 'bin', 'dependencies']) {
    const result = registry.run('source', '--dry-run', failure);
    assert.notEqual(result.status, 0, failure);
    assert.equal(result.error, undefined, `${failure}: should fail, not hang`);
  }
  const incomplete = registry.run('source', '--dry-run', 'missing');
  assert.equal(incomplete.status, 0, incomplete.stderr);
  assert.equal(JSON.parse(readFileSync(registry.receipt)).version, null);
  assert.equal(registry.run('source').status, 0);
  for (const failure of ['tags', 'race']) {
    const result = registry.run('promote', '--apply', failure);
    assert.notEqual(result.status, 0, failure);
  }
  assert.equal(registry.read().tags['dimcode-alpha'].latest, '1.2.3-alpha.11');
  assert.notEqual(registry.run('promote', '--apply').status, 0, 'never roll a newer tag back');
  const remote = registry.read();
  remote.tags['dimcode-alpha'].latest = '1.2.3-alpha.10';
  writeFileSync(registry.remote, JSON.stringify(remote));
  for (const [mode, failure] of [['npm', 'old-install'], ['npm', 'stale'], ['mirror', 'stale'], ['mirror', 'sync']]) {
    const result = registry.run(mode, failure === 'sync' ? '--apply' : '--dry-run', failure);
    assert.notEqual(result.status, 0, `${mode}: ${failure}`);
    assert.equal(result.error, undefined, `${mode}: ${failure} should fail, not hang`);
  }
  for (const { prefix } of registry.read().installs) assert.equal(existsSync(prefix), false);
});

test('promotes a new publication after snapshotting an older alpha and an unpublished dependency', (t) => {
  const registry = nightlyRegistry(t);
  const before = registry.read();
  before.tags['dimcode-alpha'].alpha = '1.2.3-alpha.9';
  delete before.tags['dimcode-web'];
  writeFileSync(registry.remote, JSON.stringify(before));
  const snapshot = registry.run('snapshot');
  assert.equal(snapshot.status, 0, snapshot.stderr);
  assert.deepEqual(registry.read(), before);
  const published = structuredClone(before);
  published.tags['dimcode-alpha'].alpha = '1.2.3-alpha.10';
  published.tags['dimcode-web'] = { alpha: '1.2.3-alpha.10' };
  writeFileSync(registry.remote, JSON.stringify(published));
  const promoted = registry.run('promote', '--apply');
  assert.equal(promoted.status, 0, promoted.stderr);
  published.tags['dimcode-alpha'].latest = '1.2.3-alpha.10';
  assert.deepEqual(registry.read(), published);
  const installed = registry.run('npm');
  assert.equal(installed.status, 0, installed.stderr);
});

test('retries a release using its recorded package set instead of the dispatch checkout', (t) => {
  const registry = nightlyRegistry(t, platforms.slice(0, -1));
  for (const mode of ['snapshot', 'promote', 'npm', 'mirror']) {
    const result = registry.run(mode, mode === 'promote' ? '--apply' : '--dry-run');
    assert.equal(result.status, 0, result.stderr);
  }
  const { protectedTags } = JSON.parse(readFileSync(registry.receipt));
  assert.equal(Object.hasOwn(protectedTags, 'dimcode-windows-x64'), false);
  assert.equal(registry.read().installs.length, 4);
});

test('does not reuse a nightly when a native manifest comes from another commit', (t) => {
  const registry = nightlyRegistry(t);
  const root = path.dirname(registry.receipt);
  const name = 'dimcode-linux-x64';
  const directory = path.join(root, name);
  const manifestPath = path.join(directory, 'package/release-manifest.json');
  const manifest = JSON.parse(readFileSync(manifestPath));
  manifest.sourceCommit = 'b'.repeat(40);
  writeFileSync(manifestPath, JSON.stringify(manifest));
  assert.equal(spawnSync('tar', ['-czf', path.join(root, `${name}-1.2.3-alpha.10.tgz`), '-C', directory, 'package']).status, 0);
  const result = registry.run('source');
  assert.equal(result.status, 0, result.stderr);
  assert.equal(JSON.parse(readFileSync(registry.receipt)).version, null);
  assert.deepEqual(registry.read().tags, registry.initialTags);
});

test('waits out a tarball the registry publishes after its metadata', (t) => {
  const registry = nightlyRegistry(t);
  writeFileSync(registry.receipt, JSON.stringify({ sourceCommit }));
  assert.equal(registry.run('source').status, 0, 'the complete nightly is reused, not rebuilt');
  const promoted = registry.run('promote', '--apply', '', { tarballMisses: 2 });
  assert.equal(promoted.status, 0, promoted.stderr);
  const published = structuredClone(registry.initialTags);
  published['dimcode-alpha'].latest = '1.2.3-alpha.10';
  assert.deepEqual(registry.read().tags, published);
});

test('waits out a dist-tag the registry serves stale after a successful write', (t) => {
  const registry = nightlyRegistry(t);
  assert.equal(registry.run('snapshot').status, 0);
  const promoted = registry.run('promote', '--apply', '', { tagLag: 2 });
  assert.equal(promoted.status, 0, promoted.stderr);
  assert.equal(registry.read().tags['dimcode-alpha'].latest, '1.2.3-alpha.10');
});

test('fails a dist-tag that never converges and a concurrent publication that outranks it', (t) => {
  const stalled = nightlyRegistry(t);
  assert.equal(stalled.run('snapshot').status, 0);
  const stuck = stalled.run('promote', '--apply', '', { tagLag: 100_000 });
  assert.notEqual(stuck.status, 0);
  assert.equal(stuck.error, undefined, 'a tag that never converges must fail, not hang');
  assert.match(stuck.stderr, /did not reach/);

  const raced = nightlyRegistry(t);
  assert.equal(raced.run('snapshot').status, 0);
  const lost = raced.run('promote', '--apply', 'race');
  assert.notEqual(lost.status, 0);
  assert.equal(lost.error, undefined, 'a concurrent publication must fail fast, not hang');
  assert.equal(raced.read().tags['dimcode-alpha'].latest, '1.2.3-alpha.11');
});

test('refuses to rebuild on an unanswered registry but reuses a nightly a blip hid', (t) => {
  const unreadable = nightlyRegistry(t);
  writeFileSync(unreadable.receipt, JSON.stringify({ sourceCommit }));
  const failed = unreadable.run('source', '--dry-run', 'network');
  assert.notEqual(failed.status, 0, 'an unanswered registry is not an incomplete publication');
  assert.equal(failed.error, undefined);

  const blip = nightlyRegistry(t);
  writeFileSync(blip.receipt, JSON.stringify({ sourceCommit }));
  const reused = blip.run('source', '--dry-run', '', { transient: 1 });
  assert.equal(reused.status, 0, reused.stderr);
  assert.equal(JSON.parse(readFileSync(blip.receipt)).version, '1.2.3-alpha.10', 'a retried read still finds the nightly');
});

test('retries a response whose body transfer fails after its headers arrived', (t) => {
  const source = nightlyRegistry(t);
  writeFileSync(source.receipt, JSON.stringify({ sourceCommit }));
  const reused = source.run('source', '--dry-run', '', { bodyResets: 1 });
  assert.equal(reused.status, 0, reused.stderr);
  assert.equal(JSON.parse(readFileSync(source.receipt)).version, '1.2.3-alpha.10',
    'a body that ended mid-transfer is not an incomplete nightly');

  const promote = nightlyRegistry(t);
  assert.equal(promote.run('snapshot').status, 0);
  const promoted = promote.run('promote', '--apply', '', { bodyResets: 1 });
  assert.equal(promoted.status, 0, promoted.stderr);
  assert.equal(promote.read().tags['dimcode-alpha'].latest, '1.2.3-alpha.10');
});
