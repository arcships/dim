import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const script = fileURLToPath(new URL('./publish-npm.mjs', import.meta.url));
const sourceCommit = 'a'.repeat(40);
const platforms = [
  ['darwin-arm64', 'darwin', 'arm64'], ['darwin-x64', 'darwin', 'x64'],
  ['linux-arm64', 'linux', 'arm64'], ['linux-x64', 'linux', 'x64'],
  ['windows-arm64', 'win32', 'arm64'], ['windows-x64', 'win32', 'x64'],
];

function fixture(t, version = '1.2.3') {
  const root = mkdtempSync(path.join(os.tmpdir(), 'dim-npm-release-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const names = [...platforms.map(([platform]) => `dimcode-${platform}`), 'dimcode'];
  const manifest = { sourceCommit, version, npmPackages: [] };
  for (const name of names) {
    const directory = path.join(root, name);
    mkdirSync(path.join(directory, 'package'), { recursive: true });
    const metadata = {
      name, version, repository: { type: 'git', url: 'git+https://github.com/arcships/dim.git' },
    };
    if (name === 'dimcode') {
      metadata.optionalDependencies = Object.fromEntries(names.slice(0, -1).map((name) => [name, version]));
      metadata.bin = { dim: 'bin/dim.mjs', dimcode: 'bin/dim.mjs' };
    } else {
      const [, os, cpu] = platforms.find(([platform]) => name === `dimcode-${platform}`);
      metadata.os = [os];
      metadata.cpu = [cpu];
    }
    writeFileSync(path.join(directory, 'package/package.json'), JSON.stringify(metadata));
    const file = `${name}-${version}.tgz`;
    const packed = spawnSync('tar', ['-czf', path.join(root, file), '-C', directory, 'package']);
    assert.equal(packed.status, 0);
    manifest.npmPackages.push({ name, file,
      sha256: createHash('sha256').update(readFileSync(path.join(root, file))).digest('hex') });
  }
  // Registry writes are the only substituted boundary; package archives are real.
  const bin = path.join(root, 'bin');
  mkdirSync(bin);
  writeFileSync(path.join(bin, 'npm'), `#!/bin/sh
if [ "$1" = view ]; then
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
  function run({ publish = false, commit = sourceCommit, registry = 'missing' } = {}) {
    writeFileSync(path.join(root, 'release-manifest.json'), JSON.stringify(manifest));
    writeFileSync(releasePath, JSON.stringify(release));
    return spawnSync(process.execPath, [script, root, version, commit, ...(publish ? ['--publish', releasePath] : [])], {
      encoding: 'utf8', env: { ...process.env, PATH: `${bin}${path.delimiter}${process.env.PATH}`, PUBLISH_LOG: log,
        REGISTRY_MODE: registry, EXISTING_INTEGRITY: 'sha512-' + createHash('sha512').update(readFileSync(path.join(root, manifest.npmPackages[0].file))).digest('base64') },
    });
  }
  return { root, names, manifest, release, log, run };
}

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
  assert.equal(calls.length, 7);
  for (const [index, name] of release.names.entries()) {
    assert.equal(calls[index], `publish ${path.join(release.root, `${name}-1.2.3.tgz`)} --access public --tag latest --ignore-scripts --registry https://registry.npmjs.org`);
  }
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

test('prerelease versions publish only to next', (t) => {
  const release = fixture(t, '1.2.3-beta.1');
  const published = release.run({ publish: true });
  assert.equal(published.status, 0, published.stderr);
  for (const call of readFileSync(release.log, 'utf8').trim().split('\n')) {
    assert(call.includes('--tag next'));
  }
});

 test('retries matching published packages and stops before writes on mismatches or registry errors', (t) => {
  const release = fixture(t);
  const resumed = release.run({publish:true,registry:'existing'});
  assert.equal(resumed.status,0,resumed.stderr);
  const calls = readFileSync(release.log,'utf8').trim().split('\n');
  assert.equal(calls.length,6);
  assert(calls[0].includes('dimcode-darwin-x64-1.2.3.tgz'));
  writeFileSync(release.log,'');
  assert.notEqual(release.run({publish:true,registry:'mismatch'}).status,0);
  assert.equal(readFileSync(release.log,'utf8'),'');
  assert.notEqual(release.run({publish:true,registry:'fail'}).status,0);
  assert.equal(readFileSync(release.log,'utf8'),'');
});
