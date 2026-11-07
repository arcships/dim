import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import zlib from 'node:zlib';

// Assembles the OCR component bundle: ONNX Runtime, pdfium, the PP-OCRv6 small
// models and their dictionary, with the licence text of every part. The bytes
// are a function of the pins in the config file and of nothing else — entry
// order, timestamps, ownership and permissions are normalized — so the digest
// committed to runtime/runtimes.json can be reproduced and checked in CI.
//
//   node scripts/ocr-bundle.mjs --platform darwin-arm64 --cache CACHE --out DIR
//   node scripts/ocr-bundle.mjs --platform all --out DIR --json
//   node scripts/ocr-bundle.mjs --platform all --out DIR --check
//   node scripts/ocr-bundle.mjs --verify DIR
//
// `--config` names the pins (default runtime/ocr-bundle.json); `--check`
// compares each built archive with the digest in the pin file (`--pin`,
// default runtime/runtimes.json), and `--verify` does the same for a directory
// of archives this run did not build. Every upstream library is copied byte
// for byte, and nothing here signs anything: the macOS dylibs carry their
// linker ad-hoc signature as published, inspected when the pin was made
// (RuntimeManager.md §2.5), so the digest never depends on a codesign version.

const args = process.argv.slice(2);
const option = (name, fallback) => {
  const index = args.indexOf(`--${name}`);
  if (index < 0) return fallback;
  const value = args[index + 1];
  assert(value && !value.startsWith('--'), `Missing value for --${name}`);
  return value;
};
const flag = (name) => args.includes(`--${name}`);
const repeat = (name) => {
  const values = [];
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] === `--${name}`) {
      assert(args[index + 1] && !args[index + 1].startsWith('--'), `Missing value for --${name}`);
      values.push(args[index + 1]);
    }
  }
  return values;
};

const configPath = path.resolve(option('config', path.join(import.meta.dirname, '..', 'runtime', 'ocr-bundle.json')));
const outRoot = path.resolve(option('out', path.join(import.meta.dirname, '..', 'dist', 'ocr-bundle')));
const cache = path.resolve(option('cache', path.join(os.tmpdir(), 'dim-ocr-bundle-cache')));
const config = JSON.parse(readFileSync(configPath, 'utf8'));

/// The digest of a byte buffer, as every pin in this repository spells it.
const digest = (bytes) => createHash('sha256').update(bytes).digest('hex');

// ---------------------------------------------------------------- downloads

/// Reads `cache/<sha256>-<name>`, downloading `url` into it when it is missing
/// or does not match `sha256`. A mismatch after a download is a refusal, never
/// a fallback: the pin is the only authority on the bytes. The digest is part
/// of the cache name because one file name serves several contents — both
/// models are `inference.onnx`.
async function acquire(url, sha256, name) {
  mkdirSync(cache, { recursive: true });
  const file = path.join(cache, `${sha256}-${name}`);
  if (existsSync(file) && digest(readFileSync(file)) === sha256) return readFileSync(file);
  const response = await fetch(url, { redirect: 'follow' });
  assert(response.ok, `${url} answered ${response.status}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  const actual = digest(bytes);
  assert.equal(actual, sha256, `${url} is ${actual}, the pin says ${sha256}`);
  writeFileSync(file, bytes);
  return bytes;
}

// ------------------------------------------------------------- tar and zip
// Both readers and both writers are here rather than in a tool: the archive
// bytes must not depend on the tar, gzip or zip a runner happens to ship.

/// The members of a tar archive, keyed by path, with the mode each one carries.
function readTar(bytes) {
  const members = new Map();
  let offset = 0;
  let longName = null;
  while (offset + 512 <= bytes.length) {
    const header = bytes.subarray(offset, offset + 512);
    if (header.every((byte) => byte === 0)) break;
    const text = (start, length) => header.subarray(start, start + length).toString('utf8').replace(/\0.*$/s, '');
    const octal = (start, length) => parseInt(header.subarray(start, start + length).toString('ascii').replace(/\0.*$/s, '').trim() || '0', 8);
    const size = octal(124, 12);
    const type = String.fromCharCode(header[156]);
    let name = text(0, 100);
    if (header.subarray(257, 262).toString('ascii') === 'ustar') {
      const prefix = text(345, 155);
      if (prefix) name = `${prefix}/${name}`;
    }
    if (type === 'L') {
      longName = bytes.subarray(offset + 512, offset + 512 + size).toString('utf8').replace(/\0.*$/s, '');
      offset += 512 + Math.ceil(size / 512) * 512;
      continue;
    }
    if (longName) {
      name = longName;
      longName = null;
    }
    assert(!name.startsWith('/') && !name.split('/').includes('..'), `Refusing archive path ${name}`);
    if (type === '0' || type === '\0' || type === '7') {
      members.set(name, { bytes: bytes.subarray(offset + 512, offset + 512 + size), mode: octal(100, 8) });
    } else if (type === '2') {
      members.set(name, { link: text(157, 100) });
    }
    offset += 512 + Math.ceil(size / 512) * 512;
  }
  return members;
}

/// The members of a zip archive, keyed by path, inflated.
function readZip(bytes) {
  const end = bytes.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]));
  assert(end >= 0, 'No zip end of central directory');
  const count = bytes.readUInt16LE(end + 10);
  let offset = bytes.readUInt32LE(end + 16);
  const members = new Map();
  for (let index = 0; index < count; index += 1) {
    assert.equal(bytes.readUInt32LE(offset), 0x02014b50, 'Bad central directory entry');
    const method = bytes.readUInt16LE(offset + 10);
    const compressedSize = bytes.readUInt32LE(offset + 20);
    const nameLength = bytes.readUInt16LE(offset + 28);
    const extraLength = bytes.readUInt16LE(offset + 30);
    const commentLength = bytes.readUInt16LE(offset + 32);
    const external = bytes.readUInt32LE(offset + 38);
    const local = bytes.readUInt32LE(offset + 42);
    const name = bytes.subarray(offset + 46, offset + 46 + nameLength).toString('utf8');
    offset += 46 + nameLength + extraLength + commentLength;
    assert(!name.startsWith('/') && !name.split('/').includes('..'), `Refusing archive path ${name}`);
    if (name.endsWith('/')) continue;
    assert.equal(bytes.readUInt32LE(local), 0x04034b50, 'Bad local file header');
    const localName = bytes.readUInt16LE(local + 26);
    const localExtra = bytes.readUInt16LE(local + 28);
    const start = local + 30 + localName + localExtra;
    const stored = bytes.subarray(start, start + compressedSize);
    const data = method === 0 ? stored : zlib.inflateRawSync(stored);
    members.set(name, { bytes: data, mode: (external >>> 16) & 0o777 || 0o644 });
  }
  return members;
}

const DOS_TIME = (0 << 11) | (0 << 5) | 0;
const DOS_DATE = ((2000 - 1980) << 9) | (1 << 5) | 1;
const MTIME = 946684800; // 2000-01-01T00:00:00Z

/// A tar header for one regular file, ustar, owned by 0:0 at a fixed time.
function tarHeader(name, size, mode) {
  assert(Buffer.byteLength(name) <= 100, `${name} does not fit a tar header`);
  const header = Buffer.alloc(512);
  const put = (value, start, length) => header.write(value, start, length, 'utf8');
  put(name, 0, 100);
  put(`${mode.toString(8).padStart(7, '0')}\0`, 100, 8);
  put('0000000\0', 108, 8);
  put('0000000\0', 116, 8);
  put(`${size.toString(8).padStart(11, '0')}\0`, 124, 12);
  put(`${MTIME.toString(8).padStart(11, '0')}\0`, 136, 12);
  put('        ', 148, 8);
  put('0', 156, 1);
  put('ustar\0', 257, 6);
  put('00', 263, 2);
  let sum = 0;
  for (const byte of header) sum += byte;
  put(`${sum.toString(8).padStart(6, '0')}\0 `, 148, 8);
  return header;
}

/// A `.tar.gz` of `entries` (`{path, bytes, mode}`), sorted, with a fixed gzip
/// header: no name, no timestamp, no origin — the same bytes on every runner.
function writeTarGz(entries) {
  const sorted = [...entries].sort((left, right) => (left.path < right.path ? -1 : 1));
  const blocks = [];
  for (const entry of sorted) {
    const bytes = entry.bytes;
    blocks.push(tarHeader(entry.path, bytes.length, entry.mode));
    blocks.push(bytes);
    const padding = (512 - (bytes.length % 512)) % 512;
    if (padding > 0) blocks.push(Buffer.alloc(padding));
  }
  blocks.push(Buffer.alloc(1024));
  const gzipped = zlib.gzipSync(Buffer.concat(blocks), { level: 9 });
  gzipped[3] = 0; // no FNAME, FCOMMENT, FEXTRA or FHCRC
  gzipped[4] = 0; // MTIME
  gzipped[5] = 0;
  gzipped[6] = 0;
  gzipped[7] = 0;
  gzipped[8] = 0; // XFL
  gzipped[9] = 255; // unknown origin
  return gzipped;
}

/// A `.zip` of `entries`, sorted, with fixed timestamps, ownership and
/// attributes: no extra fields, so nothing depends on the zip implementation.
function writeZip(entries) {
  const sorted = [...entries].sort((left, right) => (left.path < right.path ? -1 : 1));
  const locals = [];
  const central = [];
  let offset = 0;
  for (const entry of sorted) {
    const name = Buffer.from(entry.path, 'utf8');
    const data = entry.bytes;
    const compressed = zlib.deflateRawSync(data, { level: 9 });
    const crc = zlib.crc32(data);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0, 6);
    local.writeUInt16LE(8, 8);
    local.writeUInt16LE(DOS_TIME, 10);
    local.writeUInt16LE(DOS_DATE, 12);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(compressed.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(name.length, 26);
    local.writeUInt16LE(0, 28);
    locals.push(local, name, compressed);
    const entryHeader = Buffer.alloc(46);
    entryHeader.writeUInt32LE(0x02014b50, 0);
    entryHeader.writeUInt16LE(0x0314, 4); // made by UNIX, version 2.0
    entryHeader.writeUInt16LE(20, 6);
    entryHeader.writeUInt16LE(0, 8);
    entryHeader.writeUInt16LE(8, 10);
    entryHeader.writeUInt16LE(DOS_TIME, 12);
    entryHeader.writeUInt16LE(DOS_DATE, 14);
    entryHeader.writeUInt32LE(crc, 16);
    entryHeader.writeUInt32LE(compressed.length, 20);
    entryHeader.writeUInt32LE(data.length, 24);
    entryHeader.writeUInt16LE(name.length, 28);
    entryHeader.writeUInt16LE(0, 30);
    entryHeader.writeUInt16LE(0, 32);
    entryHeader.writeUInt16LE(0, 34);
    entryHeader.writeUInt16LE(0, 36);
    entryHeader.writeUInt32LE((entry.mode << 16) >>> 0, 38);
    entryHeader.writeUInt32LE(offset, 42);
    central.push(entryHeader, name);
    offset += local.length + name.length + compressed.length;
  }
  const directory = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(0, 4);
  end.writeUInt16LE(0, 6);
  end.writeUInt16LE(sorted.length, 8);
  end.writeUInt16LE(sorted.length, 10);
  end.writeUInt32LE(directory.length, 12);
  end.writeUInt32LE(offset, 16);
  end.writeUInt16LE(0, 20);
  return Buffer.concat([...locals, directory, end]);
}

// ------------------------------------------------------------- the bundle

/// The dictionary the recognition model decodes with, as one character per
/// line, read from the `PostProcess.character_dict` block of the pinned
/// `inference.yml`. The pinned digest is of `ppocr/utils/dict/ppocrv6_dict.txt`
/// at PaddleOCR v3.7.0, so the derivation and the published file must agree.
function dictionary(yml) {
  const text = yml.toString('utf8');
  const start = text.indexOf('\n  character_dict:');
  assert(start >= 0, 'The recognition config has no character_dict');
  const characters = [];
  for (const line of text.slice(start + 1).split('\n').slice(1)) {
    if (!line.startsWith('  - ')) break;
    let value = line.slice(4);
    if (value.startsWith("'") && value.endsWith("'") && value.length >= 2) {
      value = value.slice(1, -1).replaceAll("''", "'");
    }
    characters.push(value);
  }
  assert(characters.length > 0, 'The character_dict is empty');
  return Buffer.from(`${characters.join('\n')}\n`, 'utf8');
}

/// The archive members a target keeps, by bundle path: the exact library
/// files, the licence text and the third-party notices. Only `lib/` holds
/// executables; everything else is data.
function kept(members, spec) {
  const files = new Map();
  for (const [member, target] of Object.entries(spec.files ?? {})) {
    const found = members.get(member);
    assert(found && found.bytes, `${member} is not a regular file in the archive`);
    files.set(target, { bytes: Buffer.from(found.bytes), mode: target.startsWith('lib/') ? 0o755 : 0o644 });
  }
  for (const [prefix, target] of Object.entries(spec.prefixes ?? {})) {
    for (const [member, found] of members) {
      if (!member.startsWith(prefix) || !found.bytes) continue;
      files.set(path.posix.join(target, member.slice(prefix.length)), { bytes: Buffer.from(found.bytes), mode: 0o644 });
    }
  }
  return files;
}

async function bundle(platform) {
  const target = config.targets[platform];
  assert(target, `No target ${platform}`);
  const files = new Map();
  for (const model of config.models) {
    files.set(model.path, { bytes: await acquire(model.url, model.sha256, path.basename(model.url)), mode: 0o644 });
  }
  const recConfig = files.get('models/rec.inference.yml').bytes;
  const dict = dictionary(recConfig);
  assert.equal(digest(dict), config.dictionary.sha256,
    'The derived dictionary does not match the published ppocrv6_dict.txt');
  files.set(config.dictionary.path, { bytes: dict, mode: 0o644 });
  for (const license of config.licenses) {
    files.set(license.path, { bytes: await acquire(license.url, license.sha256, path.basename(license.url)), mode: 0o644 });
  }
  for (const [name, spec] of [['onnxruntime', target.onnxruntime], ['pdfium', target.pdfium]]) {
    const bytes = await acquire(spec.url, spec.sha256, path.basename(new URL(spec.url).pathname));
    const members = spec.url.endsWith('.zip') ? readZip(bytes) : readTar(zlib.gunzipSync(bytes));
    const keep = kept(members, spec);
    for (const [member, entry] of keep) files.set(member, entry);
    console.log(`${platform} ${name}: ${members.size} entries, kept ${keep.size} files`);
  }
  const manifest = {
    bundle: config.bundle,
    version: config.version,
    target: platform,
    files: [...files]
      .sort(([left], [right]) => (left < right ? -1 : 1))
      .map(([file, entry]) => ({ path: file, bytes: entry.bytes.length, sha256: digest(entry.bytes) })),
  };
  files.set('manifest.json', { bytes: Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`, 'utf8'), mode: 0o644 });
  // One top-level directory: the unpacker strips exactly one, and the
  // directory name carries the bundle version so two versions coexist.
  const root = `ocr-${config.version}-${platform}`;
  const entries = [...files].map(([file, entry]) => ({ path: `${root}/${file}`, ...entry }));
  const archive = `${root}.${platform.startsWith('windows') ? 'zip' : 'tar.gz'}`;
  const bytes = archive.endsWith('.zip') ? writeZip(entries) : writeTarGz(entries);
  return { archive, bytes };
}

// ------------------------------------------------------------------ main

const pinPath = path.resolve(option('pin', path.join(import.meta.dirname, '..', 'runtime', 'runtimes.json')));
const pins = existsSync(pinPath) ? JSON.parse(readFileSync(pinPath, 'utf8')).ocr : null;

// `--verify DIR` is the publish side of `--check`: it hashes the archives in
// DIR — built here or downloaded from a release — against the committed pin,
// and builds nothing.
if (flag('verify')) {
  const directory = path.resolve(option('verify', ''));
  assert(pins, `${pinPath} has no ocr entry`);
  const expected = Object.entries(pins.files);
  const present = readdirSync(directory).sort();
  assert.deepEqual(present, expected.map(([, name]) => name).sort(), `${directory} holds the pinned archives`);
  for (const [platform, name] of expected) {
    const actual = digest(readFileSync(path.join(directory, name)));
    assert.equal(actual, pins.sha256[platform], `${name} is ${actual}, the pin says ${pins.sha256[platform]}`);
  }
  console.log(`Verified ${expected.length} archive(s) in ${directory} against ${pinPath}`);
} else {
  const platforms = repeat('platform');
  assert(platforms.length > 0, 'Usage: node scripts/ocr-bundle.mjs --platform <target|all> --out DIR');
  const chosen = platforms.includes('all') ? Object.keys(config.targets) : platforms;
  mkdirSync(outRoot, { recursive: true });
  const built = [];
  for (const platform of chosen) {
    const { archive, bytes } = await bundle(platform);
    const actual = digest(bytes);
    const size = bytes.length;
    writeFileSync(path.join(outRoot, archive), bytes);
    if (flag('check')) {
      assert(pins, `${pinPath} has no ocr entry`);
      assert.equal(pins.files[platform], archive, `${platform} names ${pins.files[platform]}, built ${archive}`);
      assert.equal(pins.sha256[platform], actual, `${archive} is ${actual}, the pin says ${pins.sha256[platform]}`);
    }
    console.log(`${archive} ${size} ${actual}`);
    built.push({ platform, archive, size, sha256: actual });
  }
  if (flag('check')) console.log(`Checked ${built.length} archive(s) against ${pinPath}`);
  if (flag('json')) console.log(JSON.stringify(built, null, 2));
}
