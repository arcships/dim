import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {spawnSync} from 'node:child_process';
import {chmodSync, copyFileSync, cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync,
  readdirSync, rmSync} from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const usage = 'Usage: node scripts/stage-image.mjs --native ARCHIVE --web ARCHIVE --sums FILE --version VERSION '
  + '--source-commit SHA --platform linux-x64|linux-arm64 --output DIR';
const options = {};
const args = process.argv.slice(2);
for (let index = 0; index < args.length; index += 2) {
  const name = args[index];
  const value = args[index + 1];
  assert(value !== undefined && name?.startsWith('--') && !Object.hasOwn(options, name), usage);
  options[name] = value;
}
assert(args.length % 2 === 0 && ['--native', '--web', '--sums', '--version', '--source-commit', '--platform', '--output']
  .every((name) => options[name] !== undefined) && Object.keys(options).length === 7, usage);
const version = options['--version'];
const platform = options['--platform'];
assert(/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$/.test(version),
  'Invalid version');
assert(/^[a-f0-9]{40}$/.test(options['--source-commit']), 'Invalid source commit');
const arch = {'linux-x64': 'amd64', 'linux-arm64': 'arm64'}[platform];
assert(arch, 'Unsupported platform');
const nativeArchive = path.resolve(options['--native']);
const webArchive = path.resolve(options['--web']);
const sumsFile = path.resolve(options['--sums']);
const sourceCommit = options['--source-commit'];
const output = path.resolve(options['--output']);
const rootfs = path.join(output, `rootfs-${arch}`);
assert(!existsSync(rootfs), `Destination already exists: ${rootfs}`);
assert.equal(path.basename(nativeArchive), `dimcode-${platform}-${version}.tar.gz`, 'Native archive name mismatch');
assert.equal(path.basename(webArchive), `dimcode-web-${version}.tar.gz`, 'Web archive name mismatch');

const repo = path.resolve(import.meta.dirname, '..');
const protocol = readFileSync(path.join(repo, 'crates/dim-app-server/src/protocol.rs'), 'utf8')
  .match(/pub const PROTOCOL_VERSION: u32 = (\d+);/);
assert(protocol, 'Cannot read the App Server protocol version');
const protocolVersion = Number(protocol[1]);
const digest = (file) => createHash('sha256').update(readFileSync(file)).digest('hex');

const sums = new Map();
for (const line of readFileSync(sumsFile, 'utf8').split('\n').filter(Boolean)) {
  const match = /^([a-f0-9]{64})  (.+)$/.exec(line);
  assert(match, `Invalid SHA256SUMS line: ${line}`);
  sums.set(match[2], match[1]);
}
for (const archive of [nativeArchive, webArchive]) {
  const name = path.basename(archive);
  assert(lstatSync(archive).isFile(), `${name} must be a regular file`);
  assert.equal(sums.get(name), digest(archive), `${name} does not match SHA256SUMS`);
}

const scratch = mkdtempSync(path.join(os.tmpdir(), 'dimcode-image-'));
const entryTypes = {
  l: 'symbolic link', h: 'hard link', c: 'character device', b: 'block device', p: 'fifo', s: 'socket',
};
/** Rejects links and special files before extraction. */
const checkTypes = (archive) => {
  const result = spawnSync('tar', ['-tvzf', archive], {
    encoding: 'utf8', env: {...process.env, LC_ALL: 'C'},
  });
  assert.ifError(result.error);
  assert.equal(result.status, 0, `Cannot list ${path.basename(archive)}: ${result.stderr}`);
  for (const line of result.stdout.split('\n').filter(Boolean)) {
    const type = line[0];
    assert(type === '-' || type === 'd',
      `Release archive contains a ${entryTypes[type] ?? `type ${JSON.stringify(type)}`} entry: ${line}`);
  }
};
/** Rejects archive members outside the expected root before anything is extracted. */
const checkMembers = (archive, rootName) => {
  const result = spawnSync('tar', ['-tzf', archive], {encoding: 'utf8'});
  assert.ifError(result.error);
  assert.equal(result.status, 0, `Cannot list ${path.basename(archive)}: ${result.stderr}`);
  for (const member of result.stdout.split('\n').filter(Boolean)) {
    const name = member.replace(/\/+$/, '');
    assert(name === rootName || name.startsWith(`${rootName}/`),
      `Unexpected archive member in ${path.basename(archive)}: ${member}`);
    assert(!path.isAbsolute(name) && !name.split('/').includes('..'),
      `Archive member escapes its root: ${member}`);
  }
};
const extract = (archive, rootName) => {
  checkTypes(archive);
  checkMembers(archive, rootName);
  const directory = mkdtempSync(path.join(scratch, 'extract-'));
  const result = spawnSync('tar', ['-xzf', archive, '-C', directory], {encoding: 'utf8'});
  assert.ifError(result.error);
  assert.equal(result.status, 0, `Cannot extract ${path.basename(archive)}: ${result.stderr}`);
  assert.deepEqual(readdirSync(directory), [rootName], `Unexpected archive root in ${path.basename(archive)}`);
  const root = path.join(directory, rootName);
  assert(lstatSync(root).isDirectory(), 'The archive root must be a directory');
  return root;
};

/** Every file under `root`, rejecting links and special files. */
const inventory = (root) => {
  const files = {};
  const visit = (directory) => {
    for (const entry of readdirSync(directory, {withFileTypes: true})) {
      const absolute = path.join(directory, entry.name);
      const relative = path.relative(root, absolute).split(path.sep).join('/');
      assert(relative !== '' && !relative.startsWith('../') && !path.isAbsolute(relative), 'Archive path escapes its root');
      const stat = lstatSync(absolute);
      if (stat.isSymbolicLink()) throw new Error(`Release archive contains a symbolic link: ${relative}`);
      if (stat.isDirectory()) visit(absolute);
      else {
        assert(stat.isFile(), `Release archive contains a special file: ${relative}`);
        assert.equal(stat.nlink, 1, `Release archive contains a hard link: ${relative}`);
        files[relative] = digest(absolute);
      }
    }
  };
  visit(root);
  return files;
};

const readManifest = (root) => {
  const file = path.join(root, 'release-manifest.json');
  assert(lstatSync(file).isFile(), 'Missing release-manifest.json');
  return JSON.parse(readFileSync(file, 'utf8'));
};

try {
  const nativeRoot = extract(nativeArchive, `dimcode-${platform}`);
  const webRoot = extract(webArchive, 'dimcode-web');
  const nativeManifest = readManifest(nativeRoot);
  const webManifest = readManifest(webRoot);

  assert.equal(nativeManifest.schemaVersion, 1, 'Unknown native release schema');
  assert.equal(nativeManifest.platform, platform, 'Native platform mismatch');
  assert.equal(nativeManifest.version, version, 'Native version mismatch');
  assert.equal(nativeManifest.sourceCommit, sourceCommit, 'Native source mismatch');
  assert.equal(webManifest.schemaVersion, 1, 'Unknown web release schema');
  assert.equal(webManifest.platform, 'web', 'Web platform mismatch');
  assert.equal(webManifest.version, version, 'Web version mismatch');
  assert.equal(webManifest.sourceCommit, sourceCommit, 'Web source mismatch');
  assert.equal(webManifest.protocolVersion, protocolVersion, 'Web protocol mismatch');

  const nativeFiles = inventory(nativeRoot);
  delete nativeFiles['release-manifest.json'];
  assert.deepEqual(nativeFiles, nativeManifest.files, 'Native release content changed');
  const webFiles = inventory(webRoot);
  delete webFiles['release-manifest.json'];
  assert.deepEqual(webFiles, webManifest.files, 'Web release content changed');

  const binary = path.join(nativeRoot, 'bin', 'dimcode');
  assert(lstatSync(binary).isFile(), 'Missing bin/dimcode');
  assert((lstatSync(binary).mode & 0o111) !== 0, 'bin/dimcode is not executable');
  for (const relative of ['default-plugins.json', 'web/index.html', 'web/dimcode-web.json', 'web/THIRD-PARTY-NOTICES.txt']) {
    const file = path.join(relative.startsWith('web/') ? webRoot : nativeRoot, relative);
    assert(lstatSync(file).isFile(), `Missing ${relative}`);
  }
  const webDescription = JSON.parse(readFileSync(path.join(webRoot, 'web', 'dimcode-web.json'), 'utf8'));
  assert.equal(webDescription.version, version, 'Web assets version mismatch');
  assert.equal(webDescription.protocolVersion, protocolVersion, 'Web assets protocol mismatch');
  assert.equal(webDescription.sourceCommit, sourceCommit, 'Web assets source mismatch');

  const image = path.join(rootfs, 'opt', 'dim');
  mkdirSync(path.join(rootfs, 'opt'), {recursive: true});
  cpSync(nativeRoot, image, {recursive: true});
  cpSync(path.join(webRoot, 'web'), path.join(image, 'web'), {recursive: true});
  copyFileSync(path.join(webRoot, 'release-manifest.json'), path.join(image, 'web-release-manifest.json'));
  chmodSync(path.join(image, 'bin', 'dimcode'), 0o755);
  console.log(`Staged ${platform} rootfs for ${version} with protocol ${protocolVersion}.`);
} catch (error) {
  rmSync(rootfs, {recursive: true, force: true});
  throw error;
} finally {
  rmSync(scratch, {recursive: true, force: true});
}
