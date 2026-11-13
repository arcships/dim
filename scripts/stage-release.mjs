import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { chmodSync, copyFileSync, cpSync, existsSync, lstatSync, mkdirSync, readFileSync, readdirSync,
  rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { buildPlatform, checkArchitecture } from './native-binary.mjs';

const [binaryDirectory, destination, version, sourceCommit, ...rest] = process.argv.slice(2);
const usage = 'Usage: node scripts/stage-release.mjs BIN_DIR DESTINATION VERSION SOURCE_COMMIT --plugins DIR --rust-notices FILE [--account]';
assert(binaryDirectory && destination && version && sourceCommit, usage);
let account = false;
let plugins;
let rustNotices;
for (let index = 0; index < rest.length; index += 1) {
  if (rest[index] === '--account' && !account) account = true;
  else if (rest[index] === '--plugins' && plugins === undefined && rest[index + 1]) plugins = path.resolve(rest[++index]);
  else if (rest[index] === '--rust-notices' && rustNotices === undefined && rest[index + 1]) rustNotices = path.resolve(rest[++index]);
  else assert.fail(usage);
}
assert(plugins !== undefined, '--plugins DIR is required: a payload without the default plugins is not a release payload');
assert(existsSync(path.join(plugins, 'default-plugins.json')), 'The plugins directory has no default-plugins.json');
assert(rustNotices !== undefined && readFileSync(rustNotices, 'utf8').trim().length > 0,
  '--rust-notices FILE is required: generate Rust dependency notices before staging');
assert(/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$/.test(version), 'Invalid version');
assert(/^[a-f0-9]{40}$/.test(sourceCommit), 'Invalid source commit');
const { platform, host, osName, cpu } = buildPlatform();
const repo = path.resolve(import.meta.dirname, '..');
const productVersion = readFileSync(path.join(repo, 'Cargo.toml'), 'utf8').match(/\[workspace.package\]\s+version = "([^"]+)"/)[1];
assert.equal(version, productVersion, 'Version must match Cargo workspace');
assert.equal(version, JSON.parse(readFileSync(path.join(repo, 'npm/package.json'))).version, 'Version must match npm wrapper');
const storage = readFileSync(path.join(repo, 'crates/dim-app-server/src/storage/sqlite.rs'), 'utf8');
const [, upgradeCount, upgrades] = storage.match(/^const UPGRADES: \[\(u32, &\[&str\]\); (\d+)\] = \[\n([\s\S]+?)^\];/m);
const storeFormats = [...upgrades.matchAll(/^    \(\s*(\d+),/gm)].map(([, format]) => Number(format));
assert.equal(storeFormats.length, Number(upgradeCount), 'Could not read every native store upgrade step');
storeFormats.push(Number(storage.match(/^pub const FORMAT: u32 = (\d+);$/m)[1]));
assert(storeFormats.every((format, index) => Number.isSafeInteger(format) && format > (storeFormats[index - 1] ?? 0)),
  'Native store formats must be strictly increasing');
const output = path.resolve(destination);
assert(!existsSync(output), 'Destination already exists');
const digest = (data) => createHash('sha256').update(data).digest('hex');
const suffix = osName === 'windows' ? '.exe' : '';
const cli = path.resolve(binaryDirectory, `dimcode${suffix}`);
assert(lstatSync(cli).isFile(), 'The native executable must be a regular file');
if (platform === host) {
  const result = spawnSync(cli, ['--version'], { encoding: 'utf8' });
  assert.ifError(result.error);
  assert.equal(result.status, 0, `dimcode --version failed: ${result.stderr}`);
  assert.equal(result.stdout.trim(), `dimcode ${version}`, 'Binary version mismatch');
} else {
  checkArchitecture(cli, osName, cpu);
}
try {
  mkdirSync(path.join(output, 'bin'), { recursive: true });
  copyFileSync(cli, path.join(output, 'bin', `dimcode${suffix}`));
  chmodSync(path.join(output, 'bin', `dimcode${suffix}`), 0o755);
  copyFileSync(path.join(repo, 'LICENSE'), path.join(output, 'LICENSE'));
  cpSync(plugins, output, { recursive: true });
  const notices = ['LICENSE', 'NOTICE', 'xai-ratatui-inline/NOTICE', 'xai-ratatui-textarea/NOTICE'];
  for (const file of notices) {
    const destination = path.join(output, 'licenses/grok-build', file);
    mkdirSync(path.dirname(destination), { recursive: true });
    copyFileSync(path.join(repo, 'vendor/grok-build', file), destination);
  }
  mkdirSync(path.join(output, 'licenses/aimux'), { recursive: true });
  copyFileSync(path.join(repo, 'vendor/aimux/LICENSE'), path.join(output, 'licenses/aimux/LICENSE'));
  copyFileSync(path.join(repo, 'NOTICE'), path.join(output, 'NOTICE'));
  copyFileSync(rustNotices, path.join(output, 'licenses/RUST-THIRD-PARTY-NOTICES.txt'));
  const toolchain = { rust: readFileSync(path.join(repo, 'rust-toolchain.toml'), 'utf8').match(/channel = "([^"]+)"/)[1] };
  const files = {};
  for (const entry of readdirSync(output, { withFileTypes: true, recursive: true })) {
    if (entry.isFile()) files[path.relative(output, path.join(entry.parentPath, entry.name)).split(path.sep).join('/')] = digest(readFileSync(path.join(entry.parentPath, entry.name)));
    else assert(entry.isDirectory(), 'Unexpected release file type');
  }
  writeFileSync(path.join(output, 'release-manifest.json'), `${JSON.stringify({ schemaVersion: 1, version, sourceCommit, platform, toolchain, account, storeFormats, files }, null, 2)}\n`);
  console.log(`Staged ${platform} ${version} (${account ? 'with Dim account' : 'API key only'}).`);
} catch (error) {
  rmSync(output, { recursive: true, force: true });
  throw error;
}
