import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { chmodSync, copyFileSync, mkdirSync, rmSync } from 'node:fs';
import path from 'node:path';

const targets = {
  'darwin-arm64': { triple: 'aarch64-apple-darwin', zig: false },
  'darwin-x64': { triple: 'x86_64-apple-darwin', zig: false },
  'linux-x64': { triple: 'x86_64-unknown-linux-gnu', zig: true, glibc: '2.28' },
  'linux-arm64': { triple: 'aarch64-unknown-linux-gnu', zig: true, glibc: '2.28' },
  'windows-x64': { triple: 'x86_64-pc-windows-gnu', zig: true },
};

const args = process.argv.slice(2);
const chosen = [];
let profile = 'release';
for (let index = 0; index < args.length; index += 1) {
  if (args[index] === '--target') chosen.push(args[++index]);
  else if (args[index] === '--debug') profile = 'debug';
  else assert.fail(`Usage: node scripts/build-all.mjs [--target ${Object.keys(targets).join('|')}]... [--debug]`);
}
for (const name of chosen) assert(name in targets, `Unknown target ${name}; use one of ${Object.keys(targets).join(', ')}`);
const platforms = chosen.length > 0 ? chosen : Object.keys(targets);
const repo = path.resolve(import.meta.dirname, '..');
// One stamp for this build: the artifacts differ from any other build of the
// same source, which is what a client compares with its daemon.
const buildId = randomUUID();

const failed = [];
for (const platform of platforms) {
  const { triple, zig, glibc } = targets[platform];
  console.log(`\n▸ ${platform} (${triple})`);
  const command = zig ? 'zigbuild' : 'build';
  const result = spawnSync('cargo', [
    command, '--locked', ...(profile === 'release' ? ['--release'] : []),
    '--target', glibc ? `${triple}.${glibc}` : triple,
    '-p', 'dimcode',
  ], { cwd: repo, stdio: 'inherit', env: { ...process.env, DIM_BUILD_ID: buildId } });
  if (result.status !== 0) {
    failed.push(platform);
    continue;
  }
  const suffix = platform.startsWith('windows-') ? '.exe' : '';
  const file = path.join(repo, 'dist', `dimcode-${platform}${suffix}`);
  mkdirSync(path.dirname(file), { recursive: true });
  rmSync(file, { force: true });
  copyFileSync(path.join(repo, 'target', triple, profile, `dimcode${suffix}`), file);
  chmodSync(file, 0o755);
  console.log(`✓ dist/dimcode-${platform}${suffix}`);
}
if (failed.length > 0) {
  console.error(`\nFailed: ${failed.join(', ')}`);
  process.exit(1);
}
console.log(`\nBuilt ${platforms.join(', ')} (${profile}) into dist/.`);
