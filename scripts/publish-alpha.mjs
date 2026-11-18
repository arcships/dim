import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { mkdirSync, readdirSync, rmSync, statfsSync } from 'node:fs';
import path from 'node:path';
import { currentVersion, nextAlphaVersion, setAlphaVersion } from './alpha-version.mjs';

const args = process.argv.slice(2);
const usage = 'Usage: node scripts/publish-alpha.mjs [X.Y.Z-alpha.N] [--dry-run] [--platform PLATFORM ...]';
const dryRun = args.includes('--dry-run');
const selected = [];
const requested = [];
for (let index = 0; index < args.length; index += 1) {
  if (args[index] === '--dry-run') continue;
  if (args[index] === '--platform') selected.push(args[++index]);
  else requested.push(args[index]);
}
assert(requested.length <= 1 && !requested[0]?.startsWith('-') && selected.every(Boolean), usage);
assert.equal(process.platform, 'darwin', 'Alpha builds need a macOS host for the macOS targets');

const repo = path.resolve(import.meta.dirname, '..');
const targets = {
  'darwin-arm64': { triple: 'aarch64-apple-darwin' },
  'darwin-x64': { triple: 'x86_64-apple-darwin' },
  'linux-arm64': { triple: 'aarch64-unknown-linux-gnu', zig: '.2.28' },
  'linux-x64': { triple: 'x86_64-unknown-linux-gnu', zig: '.2.28' },
  'windows-arm64': { triple: 'aarch64-pc-windows-gnullvm', zig: '' },
  'windows-x64': { triple: 'x86_64-pc-windows-gnu', zig: '' },
};
assert(selected.every((platform) => platform in targets) && new Set(selected).size === selected.length, usage);
const platforms = selected.length ? Object.keys(targets).filter((platform) => selected.includes(platform)) : Object.keys(targets);
const versionFiles = ['Cargo.toml', 'Cargo.lock', 'npm/package.json', 'runtime/package.json', 'runtime/package-lock.json'];

const run = (command, commandArgs, options = {}) => {
  console.log(`$ ${command} ${commandArgs.join(' ')}`);
  const result = spawnSync(command, commandArgs, { cwd: repo, stdio: 'inherit', ...options });
  assert.ifError(result.error);
  assert.equal(result.status, 0, `${command} ${commandArgs[0]} failed`);
};
const output = (command, commandArgs) => {
  const result = spawnSync(command, commandArgs, { cwd: repo, encoding: 'utf8' });
  assert.ifError(result.error);
  return result;
};
const version = nextAlphaVersion(requested[0]);

const dirty = output('git', ['status', '--porcelain']).stdout.trim();
assert(dryRun || dirty === '', 'Commit or stash local changes first: the build must match its source commit');
assert(!versionFiles.some((file) => dirty.split('\n').some((line) => line.slice(3) === file)),
  'Version files have uncommitted changes');

const staging = path.join(repo, 'staging', `alpha-${version}`);
const remove = (directory) => rmSync(directory, { recursive: true, force: true });
mkdirSync(path.join(repo, 'staging'), { recursive: true });
for (const entry of readdirSync(path.join(repo, 'staging'), { withFileTypes: true }).filter((entry) => entry.isDirectory())) {
  if (entry.name.startsWith('alpha-')) remove(path.join(repo, 'staging', entry.name));
}
const ids = [process.env.DIM_OAUTH_CLIENT_ID_CN, process.env.DIM_OAUTH_CLIENT_ID_GLOBAL].filter(Boolean).length;
assert(ids !== 1, 'Set DIM_OAUTH_CLIENT_ID_CN and DIM_OAUTH_CLIENT_ID_GLOBAL together');
const account = ids === 2;
console.log(`\ndimcode ${currentVersion} → ${version} (dimcode-alpha on npm tag alpha${dryRun ? ', dry run' : ''}, ${account ? 'with' : 'without'} Dim account, ${platforms.join(' ')})\n`);

const sourceCommit = output('git', ['rev-parse', 'HEAD']).stdout.trim();
// The artifact stamp: a rebuilt binary of the same version and commit is still
// another build, and a client must be able to tell the daemon apart from it.
const buildId = randomUUID();
const bumped = version !== currentVersion;
try {
  if (bumped) setAlphaVersion(version);
  const packages = path.join(staging, 'complete', 'npm');
  const { bavail, bsize } = statfsSync(repo);
  assert(bavail * bsize >= 8 * 2 ** 30,
    `Only ${(bavail * bsize / 2 ** 30).toFixed(1)} GiB free; a release build needs about 8 GiB`);
  // The default plugins are the same for every platform: stage them once and
  // give every platform's payload the copy, as the release workflow does on
  // whichever runner builds a platform. --portable refuses a plugin whose
  // lock names a platform-specific package, which one shared tree cannot
  // carry for another platform; the release workflow stages per runner and
  // does not need it.
  const plugins = path.join(staging, 'plugins');
  run('node', ['scripts/stage-plugins.mjs', plugins, '--portable']);
  const rustNotices = path.join(staging, 'rust-notices.txt');
  run('node', ['scripts/rust-notices.mjs', rustNotices]);
  for (const platform of platforms) {
    const { triple, zig } = targets[platform];
    console.log(`\n▸ ${platform}`);
    run('rustup', ['target', 'add', triple], { stdio: ['ignore', 'ignore', 'inherit'] });
    run('cargo', [zig === undefined ? 'build' : 'zigbuild', '--release', '--locked',
      '--target', `${triple}${zig ?? ''}`, '-p', 'dimcode'],
    { cwd: repo, env: { ...process.env, DIM_RELEASE_BUILD: '1', DIM_BUILD_ID: buildId } });
    run('node', ['scripts/stage-release.mjs', path.join(repo, 'target', triple, 'release'),
      path.join(staging, 'stage', platform), version, sourceCommit, ...(account ? ['--account'] : []),
      '--plugins', plugins, '--rust-notices', rustNotices],
    { env: { ...process.env, DIM_BUILD_PLATFORM: platform } });
    run('node', ['scripts/pack-release.mjs', path.join(staging, 'stage', platform),
      path.join(staging, 'platforms', `release-${platform}`)]);
    remove(path.join(repo, 'target', triple));
    remove(path.join(staging, 'stage', platform));
  }
  run('node', ['scripts/stage-web.mjs', path.join(staging, 'platforms', 'release-web'), version, sourceCommit]);
  run('node', ['scripts/collect-release.mjs', path.join(staging, 'platforms'), path.join(staging, 'complete'),
    ...(platforms.length < Object.keys(targets).length ? ['--platforms', platforms.join(',')] : [])]);
  remove(path.join(staging, 'platforms'));
  remove(path.join(staging, 'complete', 'native'));
  if (dryRun) {
    console.log(`\nDry run complete: packages in ${path.relative(repo, packages)}; nothing published.`);
  } else {
    run('node', ['scripts/publish-npm.mjs', packages, version, sourceCommit, '--publish-local']);
    spawnSync('npm', ['view', 'dimcode-alpha', 'dist-tags', '--registry', 'https://registry.npmjs.org'], { cwd: repo, stdio: 'inherit' });
    console.log(`\nPublished dimcode-alpha@${version} from ${sourceCommit}. Install with: npm i -g dimcode-alpha@alpha, then run dima`);
  }
} finally {
  if (bumped) run('git', ['checkout', '--', ...versionFiles]);
  if (!dryRun) {
    remove(staging);
    for (const platform of platforms) remove(path.join(repo, 'target', targets[platform].triple));
    if (readdirSync(path.join(repo, 'staging')).length === 0) remove(path.join(repo, 'staging'));
  }
  if (spawnSync('mbx', ['--version'], { stdio: 'ignore' }).status === 0) {
    console.log('\n$ mbx gc');
    spawnSync('mbx', ['gc'], { cwd: repo, stdio: 'inherit' });
  }
}
