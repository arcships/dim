import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { isDeepStrictEqual } from 'node:util';
import { pollMilliseconds, settleMilliseconds } from './registry-wait.mjs';

const [receipt, mode, flag, ...extra] = process.argv.slice(2);
assert(receipt && ['source', 'snapshot', 'promote', 'npm', 'mirror'].includes(mode)
  && [undefined, '--apply', '--dry-run'].includes(flag) && extra.length === 0,
  'Usage: node scripts/check-alpha-registry.mjs RECEIPT source|snapshot|promote|npm|mirror [--apply|--dry-run]');
assert(flag !== '--apply' || ['promote', 'mirror'].includes(mode), 'Only promotion and mirror sync write remote state');
const state = JSON.parse(readFileSync(receipt));
const wrapper = 'dimcode-alpha';
const names = state.npmPackages?.map(({ name }) => name)
  ?? [...Object.keys(JSON.parse(readFileSync(new URL('../npm/package.json', import.meta.url))).optionalDependencies), wrapper];
const upstream = 'https://registry.npmjs.org';
const registry = mode === 'mirror' ? 'https://registry.npmmirror.com' : 'https://registry.npmjs.org';
const alpha = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)-alpha\.(0|[1-9]\d*)$/;

/**
 * A registry response whose body has already arrived, or null when the read
 * failed. A dropped connection, a deadline that expires, 429 and 5xx say
 * nothing about what the registry holds, so they are not an answer; every
 * other status is returned for the caller to interpret.
 *
 * `fetch` resolves as soon as the headers arrive, so the transfer that can
 * still fail is the body's: a connection reset mid-download, or a deadline
 * expiring while a large tarball streams, rejects in `consume` and not in
 * `fetch`. `consume` therefore runs inside this boundary, and it does
 * transport only: a body that arrived is decoded by the caller, so a truncated
 * or malformed payload stays the caller's error rather than being retried.
 */
async function read(url, timeout, consume = response => response.text()) {
  try {
    const response = await fetch(url, { signal: AbortSignal.timeout(timeout) });
    if (response.status === 429 || response.status >= 500) {
      console.error(`${url}: HTTP ${response.status}`);
      return null;
    }
    return { ok: response.ok, status: response.status, body: await consume(response) };
  } catch (error) {
    console.error(`${url}: ${error.message}`);
    return null;
  }
}

/**
 * A dist-tags document, read until the registry answers. A 429, a 5xx and a
 * failed transfer say nothing about what the registry holds, so they are asked
 * again inside `deadline`, the same window a publication is waited out; every
 * other status is the registry's answer, and `allowMissing` turns a 404 into
 * "no tags". A body that arrived is decoded here, so a malformed document stays
 * an error of this run rather than a read that is worth repeating. Each attempt
 * only gets the time left, so a registry that stops answering costs its caller
 * one window rather than one per read.
 */
async function json(url, allowMissing = false, deadline = Date.now() + settleMilliseconds) {
  for (;;) {
    const document = await read(url, Math.min(30_000, Math.max(1, deadline - Date.now())));
    if (document !== null) {
      if (allowMissing && document.status === 404) return {};
      assert(document.ok, `${url}: HTTP ${document.status}`);
      return JSON.parse(document.body);
    }
    assert(Date.now() < deadline, `${url}: the registry did not answer within ${settleMilliseconds} ms`);
    console.log(`${url}: reading again in ${pollMilliseconds} ms`);
    await delay(Math.min(pollMilliseconds, deadline - Date.now()));
  }
}

async function tags(name, deadline) {
  return json(`${upstream}/-/package/${name}/dist-tags`, true, deadline);
}

const protectedTags = (name, values) => Object.fromEntries(Object.entries(values)
  .filter(([tag]) => name === 'dimcode' || (name === wrapper ? !['latest', 'alpha'].includes(tag) : tag !== 'alpha')));

async function snapshot() {
  const deadline = Date.now() + settleMilliseconds;
  const all = Object.fromEntries(await Promise.all(['dimcode', ...names].map(async name => [name, await tags(name, deadline)])));
  state.npmPackages ??= names.map(name => ({ name }));
  state.protectedTags = Object.fromEntries(Object.entries(all).map(([name, values]) => [name, protectedTags(name, values)]));
  state.wrapperTags = all[wrapper];
  console.log(`Recorded ${wrapper} tags: ${JSON.stringify(state.wrapperTags)}`);
}

async function checkProtectedTags() {
  assert(state.protectedTags, 'Missing pre-publication dist-tag snapshot');
  const deadline = Date.now() + settleMilliseconds;
  for (const [name, expected] of Object.entries(state.protectedTags)) {
    assert.deepEqual(protectedTags(name, await tags(name, deadline)), expected, `Protected dist-tags changed: ${name}`);
  }
}

function checkWrapper(metadata, version) {
  assert.equal(metadata.name, wrapper);
  assert.equal(metadata.version, version);
  assert.deepEqual(Object.keys(metadata.bin), ['dima']);
  assert.deepEqual(metadata.optionalDependencies, Object.fromEntries(names.filter(name => name !== wrapper).map(name => [name, version])));
}

const READY = 'ready';
const ABSENT = 'absent';
const UNSERVED = 'unserved';
const UNKNOWN = 'unknown';
const FOREIGN = 'foreign';

/**
 * Whether this version is published from this source, and if not, why.
 * `ABSENT` and `UNSERVED` are answers from a registry that has not caught up:
 * the version document is missing, or it exists while the tarball it names is
 * not served yet. `UNKNOWN` is a read that failed, which is no answer at all.
 * `FOREIGN` is a publication this run did not make and cannot wait away.
 * A value that contradicts what this run published about itself is an error.
 */
async function provenance(name, version, sourceCommit) {
  const document = await read(`${upstream}/${name}/${version}`, 30_000);
  if (document === null) {
    console.error(`Cannot read ${name}@${version}: the registry did not answer`);
    return UNKNOWN;
  }
  if (document.status === 404) return ABSENT;
  assert(document.ok, `${name}@${version}: HTTP ${document.status}`);
  const metadata = JSON.parse(document.body);
  assert.equal(metadata.name, name);
  assert.equal(metadata.version, version);
  if (name === wrapper) {
    checkWrapper(metadata, version);
    return READY;
  }
  assert(metadata.dist.tarball.startsWith(`${upstream}/${name}/-/`), `Unexpected tarball URL: ${name}`);
  const archive = await read(metadata.dist.tarball, 120_000,
    response => response.arrayBuffer());
  if (archive === null) {
    console.error(`Cannot read ${metadata.dist.tarball}: the registry did not answer`);
    return UNKNOWN;
  }
  if (archive.status === 404) return UNSERVED;
  assert(archive.ok, `${metadata.dist.tarball}: HTTP ${archive.status}`);
  const bytes = Buffer.from(archive.body);
  assert.equal(`sha512-${createHash('sha512').update(bytes).digest('base64')}`, metadata.dist.integrity, `Tarball integrity mismatch: ${name}`);
  const unpacked = spawnSync('tar', ['-xzO', 'package/release-manifest.json'], { input: bytes, maxBuffer: 16 * 1024 * 1024 });
  assert.ifError(unpacked.error);
  assert.equal(unpacked.status, 0, `Cannot read release manifest: ${name}`);
  const manifest = JSON.parse(unpacked.stdout);
  assert.equal(manifest.version, version);
  assert.equal(manifest.platform, name.slice('dimcode-'.length));
  assert(/^[a-f0-9]{40}$/.test(manifest.sourceCommit), `Invalid source commit: ${name}`);
  if (name !== 'dimcode-web') assert.equal(manifest.account, true, `Missing Dim account build: ${name}`);
  return manifest.sourceCommit === sourceCommit ? READY : FOREIGN;
}

async function complete(version, sourceCommit) {
  assert(alpha.test(version), `Expected X.Y.Z-alpha.N: ${version}`);
  assert(/^[a-f0-9]{40}$/.test(sourceCommit), 'Invalid source commit');
  for (const name of names) {
    const outcome = await provenance(name, version, sourceCommit);
    if (outcome !== READY) return outcome;
    console.log(`Verified provenance: ${name}@${version} from ${sourceCommit}`);
  }
  return READY;
}

/**
 * `complete` repeated while `retry` names the outcome, so a registry that has
 * not caught up is waited out and a wrong publication is not. Returns the last
 * outcome, which is a wait that ran out when it is still a `retry` one.
 */
async function settle(version, sourceCommit, retry) {
  const deadline = Date.now() + settleMilliseconds;
  for (;;) {
    const outcome = await complete(version, sourceCommit);
    if (!retry.includes(outcome) || Date.now() >= deadline) return outcome;
    console.log(`${version} is ${outcome} on ${upstream}; reading again in ${pollMilliseconds} ms`);
    await delay(Math.min(pollMilliseconds, deadline - Date.now()));
  }
}

/**
 * A package's dist-tags once they read `expected`. A registry may keep serving
 * the value a tag had before this run wrote it, so `replaced` names those
 * earlier values: reading one of them again is not news and is waited out.
 * Any other difference belongs to another publisher, and waiting cannot take it
 * back, so it fails at once.
 */
async function settledTags(name, expected, replaced = {}) {
  const deadline = Date.now() + settleMilliseconds;
  for (;;) {
    const document = await read(`${upstream}/-/package/${name}/dist-tags`, 30_000);
    if (document === null) {
      console.error(`Cannot read ${name} dist-tags: the registry did not answer`);
    } else {
      assert(document.ok, `${name} dist-tags: HTTP ${document.status}`);
      const actual = JSON.parse(document.body);
      if (isDeepStrictEqual(actual, expected)) return actual;
      assert(Object.entries(actual).every(([tag, value]) => value === expected[tag] || value === replaced[tag]),
        `${name} dist-tags changed concurrently: expected ${JSON.stringify(expected)}, read ${JSON.stringify(actual)}`);
      console.log(`${name} dist-tags still read ${JSON.stringify(actual)}; reading again in ${pollMilliseconds} ms`);
    }
    assert(Date.now() < deadline,
      `${name} dist-tags did not reach ${JSON.stringify(expected)} within ${settleMilliseconds} ms`);
    await delay(Math.min(pollMilliseconds, deadline - Date.now()));
  }
}

function compare(left, right) {
  assert(alpha.test(left) && alpha.test(right), `Unexpected nightly version: ${left}, ${right}`);
  const a = left.match(alpha).slice(1).map(BigInt);
  const b = right.match(alpha).slice(1).map(BigInt);
  const index = a.findIndex((part, index) => part !== b[index]);
  return index === -1 ? 0 : a[index] < b[index] ? -1 : 1;
}

function command(args, timeout = 120_000) {
  console.log(`$ npm ${args.join(' ')}`);
  const result = spawnSync('npm', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], timeout });
  assert.ifError(result.error);
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
}

if (mode === 'snapshot' || mode === 'source') {
  await snapshot();
  if (mode === 'source') {
    assert(/^[a-f0-9]{40}$/.test(state.sourceCommit), 'Invalid source commit');
    const candidates = [...new Set(['latest', 'alpha'].map(tag => state.wrapperTags[tag]).filter(Boolean))].sort(compare).reverse();
    state.version = null;
    for (const version of candidates) {
      const outcome = await settle(version, state.sourceCommit, [UNSERVED, UNKNOWN]);
      if (outcome === READY) {
        state.version = version;
        break;
      }
      assert(outcome === ABSENT || outcome === FOREIGN,
        `Cannot tell whether ${version} is reusable after ${settleMilliseconds} ms (${outcome}); dispatch again instead of rebuilding`);
    }
    console.log(state.version ? `Reuse ${state.version}; repair and acceptance are required before skipping the build.` : 'No complete nightly for this source; a new build is required.');
  }
  writeFileSync(receipt, `${JSON.stringify(state, null, 2)}\n`);
} else if (mode === 'promote') {
  const outcome = flag === '--dry-run' ? await complete(state.version, state.sourceCommit)
    : await settle(state.version, state.sourceCommit, [ABSENT, UNSERVED, UNKNOWN]);
  assert.equal(outcome, READY, `${state.version} is ${outcome} on ${upstream}, expected a complete publication from ${state.sourceCommit}`);
  await checkProtectedTags();
  const expected = await tags(wrapper);
  for (const tag of ['latest', 'alpha']) {
    if (expected[tag] !== undefined) assert(compare(expected[tag], state.version) <= 0, `${tag} already points to newer ${expected[tag]}`);
    assert(expected[tag] === state.wrapperTags[tag] || expected[tag] === state.version, `Concurrent publication changed ${tag}`);
  }
  const replaced = {};
  for (const tag of ['alpha', 'latest']) {
    const current = await settledTags(wrapper, expected, replaced);
    if (current[tag] === state.version) continue;
    if (flag !== '--apply') {
      console.log(`Would set ${wrapper}@${state.version} ${tag}; no registry write performed.`);
      continue;
    }
    command(['dist-tag', 'add', `${wrapper}@${state.version}`, tag, '--registry', upstream]);
    replaced[tag] = current[tag];
    expected[tag] = state.version;
    await settledTags(wrapper, expected, replaced);
  }
  await checkProtectedTags();
} else {
  const { version } = state;
  assert(alpha.test(version), 'Expected X.Y.Z-alpha.N');
  if (mode === 'mirror' && flag === '--apply') {
    for (const name of names) {
      const response = await fetch(`${registry}/-/package/${name}/syncs`, {
        method: 'PUT',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ skipDependencies: true }),
        signal: AbortSignal.timeout(30_000),
      });
      assert(response.ok && (await response.json()).ok, `Mirror sync failed for ${name} (HTTP ${response.status})`);
    }
  }
  const refs = names.flatMap(name => name === wrapper ? ['latest', 'alpha'].map(tag => [name, tag])
    : (mode === 'npm' ? [[name, version], [name, 'alpha']] : [[name, version]]));
  const pending = new Set(refs);
  const deadline = Date.now() + (flag === '--dry-run' ? 0 : settleMilliseconds);
  while (pending.size) {
    await Promise.all([...pending].map(async ref => {
      const [name, tag] = ref;
      const url = `${registry}/${name}/${tag}`;
      let response;
      try {
        response = await fetch(url, { signal: AbortSignal.timeout(15_000) });
      } catch (error) {
        console.error(`${url}: ${error.message}`);
        return;
      }
      if (response.status === 404 || response.status === 429 || response.status >= 500) return;
      assert(response.ok, `${url}: HTTP ${response.status}`);
      const metadata = await response.json();
      if (metadata.version !== version) return;
      assert.equal(metadata.name, name);
      if (name === wrapper) checkWrapper(metadata, version);
      pending.delete(ref);
      console.log(`Verified ${name}@${tag} = ${version} on ${registry}`);
    }));
    if (pending.size) {
      assert(Date.now() < deadline, `Visibility incomplete for ${version} on ${registry}: ${[...pending].map(ref => ref.join('@')).join(', ')}`);
      await delay(Math.min(pollMilliseconds, deadline - Date.now()));
    }
  }
  await checkProtectedTags();
  for (const spec of [wrapper, `${wrapper}@alpha`]) {
    const temporary = mkdtempSync(path.join(os.tmpdir(), 'dim-nightly-install-'));
    try {
      const prefix = path.join(temporary, 'prefix');
      command(['install', '--global', '--prefix', prefix, '--cache', path.join(temporary, 'cache'), '--include=optional',
        '--ignore-scripts', '--no-audit', '--no-fund', '--registry', registry, spec], 300_000);
      const metadata = JSON.parse(readFileSync(path.join(prefix, 'lib/node_modules', wrapper, 'package.json')));
      checkWrapper(metadata, version);
      const binary = spawnSync(path.join(prefix, 'bin/dima'), ['--version'], {
        encoding: 'utf8', timeout: 30_000, env: { ...process.env, DIMCODE_HOME: path.join(temporary, 'home') },
      });
      assert.ifError(binary.error);
      assert.equal(binary.status, 0, binary.stderr);
      assert.equal(binary.stdout.trim(), `dimcode ${version}`);
      console.log(`Installed ${spec} from ${registry}: ${binary.stdout.trim()}`);
    } finally {
      rmSync(temporary, { recursive: true, force: true });
    }
  }
  const finalTags = await json(`${registry}/-/package/${wrapper}/dist-tags`);
  for (const tag of ['latest', 'alpha']) assert.equal(finalTags[tag], version, `${tag} changed during installation`);
  await checkProtectedTags();
}
