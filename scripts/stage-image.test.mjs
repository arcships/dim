import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {spawnSync} from 'node:child_process';
import {copyFileSync, existsSync, lstatSync, linkSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {test} from 'node:test';

const root = path.resolve(import.meta.dirname, '..');
const script = path.join(root, 'scripts/stage-image.mjs');
const version = '0.1.0-alpha.1';
const commit = 'a'.repeat(40);
const protocolVersion = Number(readFileSync(path.join(root, 'crates/dim-app-server/src/protocol.rs'), 'utf8')
  .match(/pub const PROTOCOL_VERSION: u32 = (\d+);/)[1]);
const skipped = process.platform === 'win32' ? 'stage-image assembly targets Linux archives' : false;
const gnuTar = spawnSync('tar', ['--version'], {encoding: 'utf8'}).stdout.includes('GNU tar');

const digest = (file) => createHash('sha256').update(readFileSync(file)).digest('hex');

function fixture(t) {
  const temporary = mkdtempSync(path.join(os.tmpdir(), 'dim-image-test-'));
  t.after(() => rmSync(temporary, {recursive: true, force: true}));
  return temporary;
}

function payload(temporary, name, files) {
  const root = path.join(temporary, 'payloads', name);
  for (const [relative, content] of Object.entries(files)) {
    const file = path.join(root, relative);
    mkdirSync(path.dirname(file), {recursive: true});
    writeFileSync(file, content, {mode: relative === 'bin/dimcode' ? 0o755 : 0o644});
  }
  return root;
}

function filesUnder(root) {
  const files = {};
  const visit = (directory) => {
    for (const entry of readdirSync(directory, {withFileTypes: true})) {
      const absolute = path.join(directory, entry.name);
      const relative = path.relative(root, absolute).split(path.sep).join('/');
      if (entry.isDirectory()) visit(absolute);
      else files[relative] = digest(absolute);
    }
  };
  visit(root);
  return files;
}

function writeManifest(root, fields) {
  writeFileSync(path.join(root, 'release-manifest.json'),
    `${JSON.stringify({schemaVersion: 1, version, sourceCommit: commit, ...fields, files: filesUnder(root)}, null, 2)}\n`);
}

function archive(temporary, root, extra = []) {
  const name = path.basename(root);
  const file = path.join(temporary, `${name}-${version}.tar.gz`);
  const args = ['-czf', file, '-C', path.dirname(root), name, ...extra.map((entry) => `${name}/${entry}`)];
  const result = spawnSync('tar', args, {encoding: 'utf8'});
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr);
  return file;
}

function sums(temporary, archives) {
  const file = path.join(temporary, 'SHA256SUMS');
  writeFileSync(file, `${archives.map((archive) => `${digest(archive)}  ${path.basename(archive)}`).join('\n')}\n`);
  return file;
}

function release(t, options = {}) {
  const temporary = fixture(t);
  const native = payload(temporary, 'dimcode-linux-x64', {
    'bin/dimcode': '#!/bin/sh\nexit 0\n',
    'default-plugins.json': '{"plugins":{}}\n',
    'plugins/example/dim-plugin.json': '{"name":"example"}\n',
    LICENSE: 'MIT\n',
    NOTICE: 'Dim\n',
    'licenses/aimux/LICENSE': 'MIT\n',
  });
  writeManifest(native, {version: options.manifestVersion ?? version,
    sourceCommit: options.sourceCommit ?? commit, platform: options.nativePlatform ?? 'linux-x64',
    account: true, toolchain: {rust: '1.98.1'}});
  if (options.changeNative) writeFileSync(path.join(native, 'default-plugins.json'), '{"plugins":{}}\nchanged\n');
  if (options.addNative) writeFileSync(path.join(native, 'extra.txt'), 'extra\n');
  if (options.linkNative) symlinkSync('LICENSE', path.join(native, 'plugins/EXAMPLE-LINK'));
  if (options.hardNative) linkSync(path.join(native, 'LICENSE'), path.join(native, 'plugins/HARD-LINK'));
  if (options.symlinkChildNative) {
    const outside = path.join(temporary, 'outside');
    mkdirSync(outside);
    writeFileSync(path.join(outside, 'escape'), 'original\n');
    symlinkSync(outside, path.join(native, 'plugins/LINK'));
  }

  const web = payload(temporary, 'dimcode-web', {
    'web/index.html': '<!doctype html>\n',
    'web/dimcode-web.json': `${JSON.stringify({
      version: options.manifestVersion ?? version,
      protocolVersion: options.webProtocol ?? protocolVersion,
      sourceCommit: options.sourceCommit ?? commit,
    })}\n`,
    'web/THIRD-PARTY-NOTICES.txt': 'notices\n',
    LICENSE: 'MIT\n',
    NOTICE: 'Dim\n',
  });
  writeManifest(web, {version: options.manifestVersion ?? version,
    sourceCommit: options.sourceCommit ?? commit, platform: 'web',
    protocolVersion: options.webProtocol ?? protocolVersion, toolchain: {node: '24.21.0'}});

  const archives = [archive(temporary, native,
    options.symlinkChildNative ? ['plugins/LINK/escape'] : []), archive(temporary, web)];
  let checksums = sums(temporary, archives);
  if (options.badSum) {
    writeFileSync(checksums, `${'0'.repeat(64)}  ${path.basename(archives[0])}\n${digest(archives[1])}  ${path.basename(archives[1])}\n`);
  }
  return {temporary, native: archives[0], web: archives[1], sums: checksums};
}

function run(fixture, platform = 'linux-x64') {
  return spawnSync(process.execPath, [script,
    '--native', fixture.native, '--web', fixture.web, '--sums', fixture.sums,
    '--version', version, '--source-commit', commit, '--platform', platform,
    '--output', path.join(fixture.temporary, 'image'),
  ], {encoding: 'utf8'});
}

test('stages the release payload for the image', {skip: skipped}, (t) => {
  const releaseFixture = release(t);
  const result = run(releaseFixture);
  assert.equal(result.status, 0, result.stderr);
  const image = path.join(releaseFixture.temporary, 'image', 'rootfs-amd64', 'opt', 'dim');
  for (const relative of ['bin/dimcode', 'default-plugins.json', 'plugins/example/dim-plugin.json',
    'web/index.html', 'web/dimcode-web.json', 'web/THIRD-PARTY-NOTICES.txt',
    'release-manifest.json', 'web-release-manifest.json', 'LICENSE', 'NOTICE', 'licenses/aimux/LICENSE']) {
    assert.equal(readFileSync(path.join(image, relative)).length > 0, true, relative);
  }
  const binary = path.join(image, 'bin/dimcode');
  assert.equal(lstatSync(binary).mode & 0o777, 0o755);
});

test('rejects archives that do not match SHA256SUMS', {skip: skipped}, (t) => {
  const result = run(release(t, {badSum: true}));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /does not match SHA256SUMS/);
});

test('rejects a changed release file', {skip: skipped}, (t) => {
  const result = run(release(t, {changeNative: true}));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Native release content changed/);
});

test('rejects an extra file in the release payload', {skip: skipped}, (t) => {
  const result = run(release(t, {addNative: true}));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Native release content changed/);
});

test('rejects a symbolic link in the release payload', {skip: skipped}, (t) => {
  const result = run(release(t, {linkNative: true}));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /symbolic link/);
});

test('rejects a symlink before extracting its child', {skip: skipped || !gnuTar ? 'needs GNU tar' : false}, (t) => {
  const fixture = release(t, {symlinkChildNative: true});
  const outside = path.join(fixture.temporary, 'outside', 'escape');
  writeFileSync(outside, 'sentinel\n');
  const result = run(fixture);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /symbolic link/);
  assert.equal(existsSync(path.join(fixture.temporary, 'image')), false);
  assert.equal(readFileSync(outside, 'utf8'), 'sentinel\n');
});

test('rejects a different Web protocol', {skip: skipped}, (t) => {
  const result = run(release(t, {webProtocol: protocolVersion + 1}));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Web protocol mismatch/);
});

test('rejects a platform mismatch', {skip: skipped}, (t) => {
  const result = run(release(t, {nativePlatform: 'linux-arm64'}));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Native platform mismatch/);
});

test('rejects a release version mismatch', {skip: skipped}, (t) => {
  const result = run(release(t, {manifestVersion: '9.9.9'}));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Native version mismatch/);
});

test('rejects a release source mismatch', {skip: skipped}, (t) => {
  const result = run(release(t, {sourceCommit: 'b'.repeat(40)}));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Native source mismatch/);
});

test('rejects an archive with the wrong name', {skip: skipped}, (t) => {
  const fixture = release(t);
  const wrong = path.join(fixture.temporary, 'wrong.tar.gz');
  copyFileSync(fixture.native, wrong);
  const result = run({...fixture, native: wrong});
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Native archive name mismatch/);
});

test('rejects a hard link in the release payload', {skip: skipped}, (t) => {
  const result = run(release(t, {hardNative: true}));
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /hard link/);
});

test('rejects a member outside the archive root', {skip: skipped || !gnuTar ? 'needs GNU tar' : false}, (t) => {
  const fixture = release(t);
  const source = path.join(fixture.temporary, 'source');
  mkdirSync(source);
  writeFileSync(path.join(source, 'escape'), 'evil\n');
  const archive = spawnSync('tar', ['--transform=s|^escape$|dimcode-linux-x64/../escape|',
    '-czf', fixture.native, '-C', source, 'escape'], {encoding: 'utf8'});
  assert.equal(archive.status, 0, archive.stderr);
  const sums = path.join(fixture.temporary, 'SHA256SUMS');
  writeFileSync(sums, `${digest(fixture.native)}  ${path.basename(fixture.native)}\n`
    + `${digest(fixture.web)}  ${path.basename(fixture.web)}\n`);
  const result = run({...fixture, sums});
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /escapes its root/);
  assert.equal(existsSync(path.join(fixture.temporary, 'image')), false);
});
