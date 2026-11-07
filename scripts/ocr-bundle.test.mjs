import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import zlib from 'node:zlib';

// The bundle script is checked against tiny archives made by tar and zip, not
// by its own writers, and against a fixture config whose pins name them. Every
// file the script would download is already in the cache under its digest, so
// nothing here reaches the network.

const script = path.join(import.meta.dirname, 'ocr-bundle.mjs');
const fixtures = path.join(import.meta.dirname, 'fixtures', 'ocr-bundle');
const digest = (bytes) => createHash('sha256').update(bytes).digest('hex');
const fixture = (name) => readFileSync(path.join(fixtures, name));

/// The dictionary `rec.inference.yml` implies: one character per line, the
/// quoted single quote unescaped and the bare dash kept as itself.
const DICTIONARY = Buffer.from("a\nb\n'\n-\ne\n");

/// The fixture pins: two targets, one per archive format, sharing the pdfium
/// archive the way an upstream release would.
function pins() {
  const model = (name) => ({
    path: `models/${name}`,
    url: `https://fixture.invalid/${name}`,
    sha256: digest(fixture(name)),
  });
  const onnxruntime = (file) => ({
    url: `https://fixture.invalid/${file}`,
    sha256: digest(fixture(file)),
    files: {
      'onnxruntime-fixture/LICENSE': 'licenses/ONNXRUNTIME-MIT.txt',
      'onnxruntime-fixture/ThirdPartyNotices.txt': 'licenses/ONNXRUNTIME-THIRD-PARTY.txt',
      ...(file.endsWith('.zip')
        ? { 'onnxruntime-fixture/lib/onnxruntime.dll': 'lib/onnxruntime.dll' }
        : { 'onnxruntime-fixture/lib/libonnxruntime.so.1.31.1': 'lib/libonnxruntime.so.1.31.1' }),
    },
  });
  const pdfium = (library) => ({
    url: 'https://fixture.invalid/pdfium.tar.gz',
    sha256: digest(fixture('pdfium.tar.gz')),
    files: {
      [library]: library.endsWith('.dll') ? 'lib/pdfium.dll' : 'lib/libpdfium.so',
      LICENSE: 'licenses/PDFIUM-BSD-3-Clause.txt',
    },
    prefixes: { 'licenses/': 'licenses/PDFIUM-THIRD-PARTY' },
  });
  return {
    bundle: 'fixture-ocr',
    version: '1',
    models: ['det.onnx', 'det.inference.yml', 'rec.onnx', 'rec.inference.yml'].map(model),
    dictionary: { path: 'models/ppocrv6_dict.txt', sha256: digest(DICTIONARY) },
    licenses: [
      {
        path: 'licenses/LICENSE.txt',
        url: 'https://fixture.invalid/LICENSE.txt',
        sha256: digest(fixture('LICENSE.txt')),
      },
    ],
    targets: {
      'linux-x64': {
        onnxruntime: onnxruntime('ort.tar.gz'),
        pdfium: pdfium('lib/libpdfium.so'),
      },
      'windows-x64': {
        onnxruntime: onnxruntime('ort.zip'),
        pdfium: pdfium('bin/pdfium.dll'),
      },
    },
  };
}

function scratch(t) {
  const dir = mkdtempSync(path.join(os.tmpdir(), 'dim-ocr-bundle-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

/// Writes `config` and the cache it reads: every pinned file under
/// `<sha256>-<name>`, exactly where `acquire` looks first.
function prepare(dir, config) {
  const configPath = path.join(dir, 'ocr-bundle.json');
  writeFileSync(configPath, JSON.stringify(config, null, 2));
  const cache = path.join(dir, 'cache');
  mkdirSync(cache);
  const add = (url, sha256) => {
    const name = path.basename(new URL(url).pathname);
    writeFileSync(path.join(cache, `${sha256}-${name}`), fixture(name));
  };
  for (const model of config.models) add(model.url, model.sha256);
  for (const license of config.licenses) add(license.url, license.sha256);
  for (const target of Object.values(config.targets)) {
    add(target.onnxruntime.url, target.onnxruntime.sha256);
    add(target.pdfium.url, target.pdfium.sha256);
  }
  return { configPath, cache };
}

function build(dir, configPath, cache, out) {
  const result = spawnSync(process.execPath, [script, '--config', configPath, '--cache', cache,
    '--out', out, '--platform', 'all'], { encoding: 'utf8' });
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  const built = new Map();
  for (const line of result.stdout.split('\n')) {
    const match = line.match(/^(\S+) (\d+) ([0-9a-f]{64})$/);
    if (match) built.set(match[1], { bytes: Number(match[2]), sha256: match[3] });
  }
  return built;
}

test('the same pins build the same archive twice', (t) => {
  const dir = scratch(t);
  const { configPath, cache } = prepare(dir, pins());
  const first = build(dir, configPath, cache, path.join(dir, 'first'));
  const second = build(dir, configPath, cache, path.join(dir, 'second'));
  assert.deepEqual([...first.keys()].sort(), ['ocr-1-linux-x64.tar.gz', 'ocr-1-windows-x64.zip']);
  assert.deepEqual(first, second);
  for (const [name, entry] of first) {
    const bytes = readFileSync(path.join(dir, 'first', name));
    assert.equal(bytes.length, entry.bytes);
    assert.equal(digest(bytes), entry.sha256);
  }
  const tar = zlib.gunzipSync(readFileSync(path.join(dir, 'first', 'ocr-1-linux-x64.tar.gz'))).toString('utf8');
  const zip = readFileSync(path.join(dir, 'first', 'ocr-1-windows-x64.zip')).toString('utf8');
  for (const [archive, member] of [[tar, 'ocr-1-linux-x64/'], [zip, 'ocr-1-windows-x64/']]) {
    assert.ok(archive.includes(`${member}manifest.json`), member);
    assert.ok(archive.includes(`${member}lib/`), member);
    assert.ok(archive.includes(`${member}models/ppocrv6_dict.txt`), member);
    assert.ok(archive.includes(`${member}licenses/PDFIUM-THIRD-PARTY/zlib.txt`), member);
  }
  assert.ok(tar.includes('"bundle": "fixture-ocr"'));
  assert.ok(tar.includes('"target": "linux-x64"'));
});

test('a pin that the built archive does not match fails the check', (t) => {
  const dir = scratch(t);
  const { configPath, cache } = prepare(dir, pins());
  const built = build(dir, configPath, cache, path.join(dir, 'out'));
  const pin = path.join(dir, 'runtimes.json');
  const platform = (name) => name.replace(/^ocr-1-/, '').replace(/\.(tar\.gz|zip)$/, '');
  const entry = (digests) => ({
    ocr: {
      files: Object.fromEntries([...built.keys()].map((name) => [platform(name), name])),
      sha256: Object.fromEntries([...built.entries()].map(([name, value]) => [platform(name), digests ? value.sha256 : '0'.repeat(64)])),
    },
  });
  writeFileSync(pin, JSON.stringify(entry(true)));
  const checked = spawnSync(process.execPath, [script, '--config', configPath, '--cache', cache,
    '--out', path.join(dir, 'check'), '--platform', 'all', '--pin', pin, '--check'], { encoding: 'utf8' });
  assert.equal(checked.status, 0, `${checked.stdout}\n${checked.stderr}`);
  const verified = spawnSync(process.execPath, [script, '--config', configPath, '--pin', pin,
    '--verify', path.join(dir, 'check')], { encoding: 'utf8' });
  assert.equal(verified.status, 0, `${verified.stdout}\n${verified.stderr}`);
  writeFileSync(pin, JSON.stringify(entry(false)));
  const refused = spawnSync(process.execPath, [script, '--config', configPath, '--cache', cache,
    '--out', path.join(dir, 'check-again'), '--platform', 'all', '--pin', pin, '--check'], { encoding: 'utf8' });
  assert.notEqual(refused.status, 0);
  assert.match(refused.stderr, /the pin says 0{64}/);
  const refusedVerify = spawnSync(process.execPath, [script, '--config', configPath, '--pin', pin,
    '--verify', path.join(dir, 'check')], { encoding: 'utf8' });
  assert.notEqual(refusedVerify.status, 0);
  assert.match(refusedVerify.stderr, /the pin says 0{64}/);
});

test('a dictionary that does not match the published digest is refused', (t) => {
  const dir = scratch(t);
  const config = pins();
  config.dictionary.sha256 = '0'.repeat(64);
  const { configPath, cache } = prepare(dir, config);
  const result = spawnSync(process.execPath, [script, '--config', configPath, '--cache', cache,
    '--out', path.join(dir, 'out'), '--platform', 'linux-x64'], { encoding: 'utf8' });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /does not match the published ppocrv6_dict\.txt/);
});

test('a kept member that is a link is refused rather than followed', (t) => {
  const dir = scratch(t);
  const config = pins();
  config.targets['linux-x64'].onnxruntime.files = {
    'onnxruntime-fixture/lib/libonnxruntime.so': 'lib/libonnxruntime.so',
  };
  const { configPath, cache } = prepare(dir, config);
  const result = spawnSync(process.execPath, [script, '--config', configPath, '--cache', cache,
    '--out', path.join(dir, 'out'), '--platform', 'linux-x64'], { encoding: 'utf8' });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /is not a regular file in the archive/);
});
