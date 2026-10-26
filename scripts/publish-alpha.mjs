// Build and publish the Rust CLI to the npm `alpha` dist-tag in one command,
// the counterpart of the TypeScript CLI's `publish:bin`. Beta and stable
// releases go through .github/workflows/release.yml instead.
//
//   node scripts/publish-alpha.mjs               # next alpha
//   node scripts/publish-alpha.mjs 0.1.0-alpha.3 # explicit alpha
//   node scripts/publish-alpha.mjs --dry-run     # build and validate, no commit or npm write
//   node scripts/publish-alpha.mjs --platform darwin-arm64  # only these platforms (repeatable)
//
// Picks the version, commits the version bump, cross-builds all six platforms
// on this Mac (Linux and Windows through cargo-zigbuild), stages and packs them
// with the release scripts, then publishes through scripts/publish-npm.mjs.
// Rerunning after a failure resumes: an unpublished alpha already in Cargo.toml
// is reused, and packages already on npm with identical bytes are skipped.
// Disk use stays near one target at a time: each target's build directory is
// removed once its package is packed, intermediate staging is dropped as soon
// as the next step has consumed it, and a successful publish removes it all.
// When mbx is installed, `mbx gc` runs at the end, whether the run succeeded or not.
// Set DIM_OAUTH_SOURCE_DIR to a dim-oauth-plugin-rust checkout beside this one
// to publish account builds from it; without it the build is API-key only.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, readdirSync, readFileSync, rmSync, statfsSync, writeFileSync } from 'node:fs';
import path from 'node:path';

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
  // Linked against glibc 2.28 so the binaries run on older distributions.
  'linux-arm64': { triple: 'aarch64-unknown-linux-gnu', zig: '.2.28' },
  'linux-x64': { triple: 'x86_64-unknown-linux-gnu', zig: '.2.28' },
  'windows-arm64': { triple: 'aarch64-pc-windows-gnullvm', zig: '' },
  'windows-x64': { triple: 'x86_64-pc-windows-gnu', zig: '' },
};
assert(selected.every((platform) => platform in targets) && new Set(selected).size === selected.length, usage);
// Without --platform an alpha ships every platform.
const platforms = selected.length ? Object.keys(targets).filter((platform) => selected.includes(platform)) : Object.keys(targets);
// The release scripts run under `node` even when this script runs under bun:
// staging bundles the running Node and checks it against .node-version.
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
const parse = (value) => value.match(/^(\d+)\.(\d+)\.(\d+)(?:-([a-z]+)\.(\d+))?$/)?.slice(1)
  .map((part) => (/^\d+$/.test(part ?? '') ? Number(part) : part));
const compare = (left, right) => {
  const [a, b] = [parse(left), parse(right)];
  for (const index of [0, 1, 2, 4]) if (a[index] !== b[index]) return a[index] - b[index];
  return 0;
};

// Registry state decides the version, so read it before any build.
const listed = output('npm', ['view', 'dimcode', 'versions', '--json', '--registry', 'https://registry.npmjs.org']);
let published = JSON.parse(listed.stdout || '[]');
if (listed.status !== 0) {
  assert.equal(published.error?.code, 'E404', 'Cannot read dimcode versions from npm');
  published = [];
}
published = [published].flat();
const current = readFileSync(path.join(repo, 'Cargo.toml'), 'utf8')
  .match(/\[workspace\.package\]\s+version = "([^"]+)"/)[1];
const alphas = published.filter((value) => /^\d+\.\d+\.\d+-alpha\.\d+$/.test(value)).sort(compare);
let version = requested[0];
if (!version) {
  if (/-alpha\.\d+$/.test(current) && !published.includes(current)) version = current;
  else if (alphas.length === 0) version = `${current.split('-')[0]}-alpha.0`;
  else version = alphas.at(-1).replace(/\d+$/, (number) => String(Number(number) + 1));
}
assert(/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)-alpha\.(0|[1-9]\d*)$/.test(version),
  `Expected X.Y.Z-alpha.N, got ${version}`);
assert(!published.includes(version), `dimcode@${version} is already on npm`);
// publish-npm.mjs enforces this again before writing; checking here saves a full build.
const betas = published.filter((value) => /^\d+\.\d+\.\d+-beta\.\d+$/.test(value)).sort(compare);
// An alpha sorts below a beta of the same X.Y.Z, so comparing the cores is enough.
assert(betas.length === 0 || compare(version.split('-')[0], betas.at(-1).split('-')[0]) <= 0,
  `Alpha ${version} must stay below the newest dimcode beta ${betas.at(-1)}`);

const dirty = output('git', ['status', '--porcelain']).stdout.trim();
assert(dryRun || dirty === '', 'Commit or stash local changes first: the build must match its source commit');
assert(!versionFiles.some((file) => dirty.split('\n').some((line) => line.slice(3) === file)),
  'Version files have uncommitted changes');

const staging = path.join(repo, 'staging', `alpha-${version}`);
const remove = (directory) => rmSync(directory, { recursive: true, force: true });
// Leftovers of earlier alphas are never resumed.
mkdirSync(path.join(repo, 'staging'), { recursive: true });
for (const entry of readdirSync(path.join(repo, 'staging'), { withFileTypes: true }).filter((entry) => entry.isDirectory())) {
  if (entry.name.startsWith('alpha-') && entry.name !== `alpha-${version}`) remove(path.join(repo, 'staging', entry.name));
}
// The private crate reaches this checkout as its sibling `../dim`.
const account = process.env.DIM_OAUTH_SOURCE_DIR ? path.resolve(process.env.DIM_OAUTH_SOURCE_DIR) : null;
if (account) run(path.join(account, 'scripts/sync-lock.sh'), [], { env: { ...process.env, DIM_PUBLIC_REPO: repo } });
console.log(`\ndimcode ${current} → ${version} (npm tag alpha${dryRun ? ', dry run' : ''}, ${account ? 'with' : 'without'} Dim account, ${platforms.join(' ')})\n`);

const bumped = version !== current;
try {
  if (bumped) {
    const edit = (file, change) => writeFileSync(path.join(repo, file), change(readFileSync(path.join(repo, file), 'utf8')));
    const json = (file, change) => edit(file, (text) => {
      const data = JSON.parse(text);
      change(data);
      return `${JSON.stringify(data, null, 2)}\n`;
    });
    edit('Cargo.toml', (text) => text.replace(/(\[workspace\.package\]\s+version = ")[^"]+/, `$1${version}`));
    edit('npm/package.json', (text) => text.replaceAll(`"${current}"`, `"${version}"`));
    json('runtime/package.json', (data) => { data.version = version; });
    json('runtime/package-lock.json', (data) => { data.version = version; data.packages[''].version = version; });
    run('cargo', ['metadata', '--format-version', '1'], { stdio: ['ignore', 'ignore', 'inherit'] });
    if (!dryRun) run('git', ['commit', '-m', `chore: release v${version}`, '--', ...versionFiles]);
  }
  const sourceCommit = output('git', ['rev-parse', 'HEAD']).stdout.trim();
  const packages = path.join(staging, 'complete', 'npm');

  // A partial npm publish can only resume with the same bytes, so reuse the
  // packages already built from this commit instead of rebuilding them.
  let reused = false;
  try { reused = !dryRun && JSON.parse(readFileSync(path.join(packages, 'release-manifest.json'), 'utf8')).sourceCommit === sourceCommit; }
  catch {}
  if (reused) console.log(`Reusing packages built from ${sourceCommit} in ${path.relative(repo, packages)}`);
  else remove(staging);
  const { bavail, bsize } = statfsSync(repo);
  assert(reused || bavail * bsize >= 8 * 2 ** 30,
    `Only ${(bavail * bsize / 2 ** 30).toFixed(1)} GiB free; a release build needs about 8 GiB`);
  for (const platform of reused ? [] : platforms) {
    const { triple, zig } = targets[platform];
    console.log(`\n▸ ${platform}`);
    run('rustup', ['target', 'add', triple], { stdio: ['ignore', 'ignore', 'inherit'] });
    const build = account ?? repo;
    run('cargo', [zig === undefined ? 'build' : 'zigbuild', '--release', ...(account ? [] : ['--locked']),
      '--target', `${triple}${zig ?? ''}`, '-p', account ? 'dim-oauth' : 'dimcode'], { cwd: build });
    run('node', ['scripts/stage-release.mjs', path.join(build, 'target', triple, 'release'),
      path.join(staging, 'stage', platform), version, sourceCommit, ...(account ? ['--account'] : [])],
    { env: { ...process.env, DIM_BUILD_PLATFORM: platform } });
    run('node', ['scripts/pack-release.mjs', path.join(staging, 'stage', platform),
      path.join(staging, 'platforms', `release-${platform}`)]);
    remove(path.join(build, 'target', triple));
    remove(path.join(staging, 'stage', platform));
  }
  if (!reused) {
    // The frontend is built once, for every platform.
    run('node', ['scripts/stage-web.mjs', path.join(staging, 'platforms', 'release-web'), version, sourceCommit]);
    run('node', ['scripts/stage-plugins.mjs', path.join(staging, 'platforms', 'release-plugins'), version, sourceCommit]);
    run('node', ['scripts/collect-release.mjs', path.join(staging, 'platforms'), path.join(staging, 'complete'),
      ...(platforms.length < Object.keys(targets).length ? ['--platforms', platforms.join(',')] : [])]);
    remove(path.join(staging, 'platforms'));
    remove(path.join(staging, 'complete', 'native'));
  }
  if (dryRun) {
    console.log(`\nDry run complete: packages in ${path.relative(repo, packages)}; nothing committed or published.`);
  } else {
    run('node', ['scripts/publish-npm.mjs', packages, version, sourceCommit, '--publish-local']);
    run('npm', ['view', 'dimcode', 'dist-tags', '--registry', 'https://registry.npmjs.org']);
    remove(staging);
    console.log(`\nPublished dimcode@${version}. Install with: npm i -g dimcode@alpha`);
    console.log(`Push the release commit when ready: git push`);
  }
} finally {
  if (dryRun && bumped) run('git', ['checkout', '--', ...versionFiles]);
  // When mbx manages target/, let it trim its caches back to their budgets.
  // A cleanup failure never fails the release.
  if (spawnSync('mbx', ['--version'], { stdio: 'ignore' }).status === 0) {
    console.log('\n$ mbx gc');
    spawnSync('mbx', ['gc'], { cwd: repo, stdio: 'inherit' });
  }
}
