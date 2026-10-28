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
const parse = (value) => value.match(/^(\d+)\.(\d+)\.(\d+)(?:-([a-z]+)\.(\d+))?$/)?.slice(1)
  .map((part) => (/^\d+$/.test(part ?? '') ? Number(part) : part));
const compare = (left, right) => {
  const [a, b] = [parse(left), parse(right)];
  for (const index of [0, 1, 2, 4]) if (a[index] !== b[index]) return a[index] - b[index];
  return 0;
};

const versions = (name) => {
  const listed = output('npm', ['view', name, 'versions', '--json', '--registry', 'https://registry.npmjs.org']);
  const parsed = JSON.parse(listed.stdout || '[]');
  if (listed.status === 0) return [parsed].flat();
  assert.equal(parsed.error?.code, 'E404', `Cannot read ${name} versions from npm`);
  return [];
};
const published = versions('dimcode');
const released = [...published, ...versions('dimcode-alpha')];
const current = readFileSync(path.join(repo, 'Cargo.toml'), 'utf8')
  .match(/\[workspace\.package\]\s+version = "([^"]+)"/)[1];
const alphas = released.filter((value) => /^\d+\.\d+\.\d+-alpha\.\d+$/.test(value)).sort(compare);
let version = requested[0];
if (!version) {
  if (/-alpha\.\d+$/.test(current) && !released.includes(current)) version = current;
  else if (alphas.length === 0) version = `${current.split('-')[0]}-alpha.0`;
  else version = alphas.at(-1).replace(/\d+$/, (number) => String(Number(number) + 1));
}
assert(/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)-alpha\.(0|[1-9]\d*)$/.test(version),
  `Expected X.Y.Z-alpha.N, got ${version}`);
assert(!released.includes(version), `${version} is already on npm`);
const betas = published.filter((value) => /^\d+\.\d+\.\d+-beta\.\d+$/.test(value)).sort(compare);
assert(betas.length === 0 || compare(version.split('-')[0], betas.at(-1).split('-')[0]) <= 0,
  `Alpha ${version} must stay below the newest dimcode beta ${betas.at(-1)}`);

const dirty = output('git', ['status', '--porcelain']).stdout.trim();
assert(dryRun || dirty === '', 'Commit or stash local changes first: the build must match its source commit');
assert(!versionFiles.some((file) => dirty.split('\n').some((line) => line.slice(3) === file)),
  'Version files have uncommitted changes');

const staging = path.join(repo, 'staging', `alpha-${version}`);
const remove = (directory) => rmSync(directory, { recursive: true, force: true });
mkdirSync(path.join(repo, 'staging'), { recursive: true });
for (const entry of readdirSync(path.join(repo, 'staging'), { withFileTypes: true }).filter((entry) => entry.isDirectory())) {
  if (entry.name.startsWith('alpha-') && entry.name !== `alpha-${version}`) remove(path.join(repo, 'staging', entry.name));
}
const ids = [process.env.DIM_OAUTH_CLIENT_ID_CN, process.env.DIM_OAUTH_CLIENT_ID_GLOBAL].filter(Boolean).length;
assert(ids !== 1, 'Set DIM_OAUTH_CLIENT_ID_CN and DIM_OAUTH_CLIENT_ID_GLOBAL together');
const account = ids === 2;
console.log(`\ndimcode ${current} → ${version} (dimcode-alpha on npm tag alpha${dryRun ? ', dry run' : ''}, ${account ? 'with' : 'without'} Dim account, ${platforms.join(' ')})\n`);

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
    run('cargo', [zig === undefined ? 'build' : 'zigbuild', '--release', '--locked',
      '--target', `${triple}${zig ?? ''}`, '-p', 'dimcode'], { cwd: repo });
    run('node', ['scripts/stage-release.mjs', path.join(repo, 'target', triple, 'release'),
      path.join(staging, 'stage', platform), version, sourceCommit, ...(account ? ['--account'] : [])],
    { env: { ...process.env, DIM_BUILD_PLATFORM: platform } });
    run('node', ['scripts/pack-release.mjs', path.join(staging, 'stage', platform),
      path.join(staging, 'platforms', `release-${platform}`)]);
    remove(path.join(repo, 'target', triple));
    remove(path.join(staging, 'stage', platform));
  }
  if (!reused) {
    run('node', ['scripts/stage-web.mjs', path.join(staging, 'platforms', 'release-web'), version, sourceCommit]);
    run('node', ['scripts/collect-release.mjs', path.join(staging, 'platforms'), path.join(staging, 'complete'),
      ...(platforms.length < Object.keys(targets).length ? ['--platforms', platforms.join(',')] : [])]);
    remove(path.join(staging, 'platforms'));
    remove(path.join(staging, 'complete', 'native'));
  }
  if (dryRun) {
    console.log(`\nDry run complete: packages in ${path.relative(repo, packages)}; nothing committed or published.`);
  } else {
    run('node', ['scripts/publish-npm.mjs', packages, version, sourceCommit, '--publish-local']);
    spawnSync('npm', ['view', 'dimcode-alpha', 'dist-tags', '--registry', 'https://registry.npmjs.org'], { cwd: repo, stdio: 'inherit' });
    remove(staging);
    console.log(`\nPublished dimcode-alpha@${version}. Install with: npm i -g dimcode-alpha@alpha, then run dima`);
    console.log(`Push the release commit when ready: git push`);
  }
} finally {
  if (dryRun && bumped) run('git', ['checkout', '--', ...versionFiles]);
  if (spawnSync('mbx', ['--version'], { stdio: 'ignore' }).status === 0) {
    console.log('\n$ mbx gc');
    spawnSync('mbx', ['gc'], { cwd: repo, stdio: 'inherit' });
  }
}
