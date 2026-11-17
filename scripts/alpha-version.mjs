import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';

const repo = path.resolve(import.meta.dirname, '..');
export const currentVersion = readFileSync(path.join(repo, 'Cargo.toml'), 'utf8')
  .match(/\[workspace\.package\]\s+version = "([^"]+)"/)[1];
const alphaVersion = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)-alpha\.(0|[1-9]\d*)$/;
const parse = (value) => value.match(/^(\d+)\.(\d+)\.(\d+)(?:-([a-z]+)\.(\d+))?$/)?.slice(1)
  .map((part) => (/^\d+$/.test(part ?? '') ? Number(part) : part));
const compare = (left, right) => {
  const [a, b] = [parse(left), parse(right)];
  for (const index of [0, 1, 2, 4]) if (a[index] !== b[index]) return a[index] - b[index];
  return 0;
};

const versions = (name) => {
  const listed = spawnSync('npm', ['view', name, 'versions', '--json', '--registry', 'https://registry.npmjs.org'], { encoding: 'utf8' });
  assert.ifError(listed.error);
  const parsed = JSON.parse(listed.stdout || '[]');
  if (listed.status === 0) return [parsed].flat();
  assert.equal(parsed.error?.code, 'E404', `Cannot read ${name} versions from npm`);
  return [];
};
export function nextAlphaVersion(requested, reserved = []) {
  const published = versions('dimcode');
  const { optionalDependencies } = JSON.parse(readFileSync(path.join(repo, 'npm/package.json')));
  const packages = ['dimcode-alpha', ...Object.keys(optionalDependencies)];
  const released = [...reserved, ...published, ...packages.flatMap(versions)];
  const alphas = released.filter((value) => /^\d+\.\d+\.\d+-alpha\.\d+$/.test(value)).sort(compare);
  let version = requested;
  if (!version) {
    if (alphas.length === 0) version = `${currentVersion.split('-')[0]}-alpha.0`;
    else version = alphas.at(-1).replace(/\d+$/, (number) => String(Number(number) + 1));
  }
  assert(alphaVersion.test(version),
    `Expected X.Y.Z-alpha.N, got ${version}`);
  assert(!released.includes(version), `${version} is already on npm`);
  const betas = published.filter((value) => /^\d+\.\d+\.\d+-beta\.\d+$/.test(value)).sort(compare);
  assert(betas.length === 0 || compare(version.split('-')[0], betas.at(-1).split('-')[0]) <= 0,
    `Alpha ${version} must stay below the newest dimcode beta ${betas.at(-1)}`);
  return version;
}

/**
 * The product fields of `root` and the version each carries, named as a refusal
 * reports them. These are the fields a release PR synchronizes: one version is
 * shared by the executable, the eight npm packages and both runtime files.
 */
function productVersionFields(root) {
  const cargo = readFileSync(path.join(root, 'Cargo.toml'), 'utf8')
    .match(/\[workspace\.package\]\s+version = "([^"]+)"/)[1];
  const wrapper = JSON.parse(readFileSync(path.join(root, 'npm/package.json')));
  const runtime = JSON.parse(readFileSync(path.join(root, 'runtime/package.json')));
  const runtimeLock = JSON.parse(readFileSync(path.join(root, 'runtime/package-lock.json')));
  return [
    ['Cargo.toml [workspace.package] version', cargo],
    ['npm/package.json version', wrapper.version],
    ...Object.entries(wrapper.optionalDependencies)
      .map(([name, value]) => [`npm/package.json optionalDependencies ${name}`, value]),
    ['runtime/package.json version', runtime.version],
    ['runtime/package-lock.json version', runtimeLock.version],
    ['runtime/package-lock.json packages[""] version', runtimeLock.packages[''].version],
  ];
}

/**
 * The product fields of `root` that do not carry `version`, each as
 * `<field> says <committed>`; empty when the checkout carries it everywhere.
 */
export function versionMismatches(root, version) {
  return productVersionFields(root)
    .filter(([, committed]) => committed !== version)
    .map(([field, committed]) => `${field} says ${committed}`);
}

/**
 * Refuses `version` unless `root` carries it in every product field, and names
 * the two remedies. A version that is already tagged or published never equals
 * the allocated one: the allocator adds one to the highest alpha that is tagged
 * or on npm, so equality already excludes that case and the refusal names it
 * anyway.
 */
export function assertReleaseVersion(root, version) {
  const mismatches = versionMismatches(root, version);
  if (mismatches.length === 0) return version;
  throw new Error(`This main commit does not record ${version}: ${mismatches.join('; ')}. `
    + `Merge the release PR that commits ${version} into main, then dispatch again. `
    + `For recovery, dispatch an existing tag instead: -f tag=v${version}. `
    + `A version that is already tagged or published is rebuilt from its tag, never allocated again.`);
}

export function setAlphaVersion(version) {
  assert(alphaVersion.test(version), 'Expected an alpha version');
  if (version === currentVersion) return;
  const edit = (file, change) => writeFileSync(path.join(repo, file), change(readFileSync(path.join(repo, file), 'utf8')));
  const json = (file, change) => edit(file, (text) => {
    const data = JSON.parse(text);
    change(data);
    return `${JSON.stringify(data, null, 2)}\n`;
  });
  edit('Cargo.toml', (text) => text.replace(/(\[workspace\.package\]\s+version = ")[^"]+/, `$1${version}`));
  edit('npm/package.json', (text) => text.replaceAll(`"${currentVersion}"`, `"${version}"`));
  json('runtime/package.json', (data) => { data.version = version; });
  json('runtime/package-lock.json', (data) => { data.version = version; data.packages[''].version = version; });
  execFileSync('cargo', ['metadata', '--format-version', '1'], { cwd: repo, stdio: ['ignore', 'ignore', 'inherit'] });
}

if (import.meta.main) {
  const [first, second] = process.argv.slice(2);
  if (first === '--verify') {
    assert(second, 'Usage: node scripts/alpha-version.mjs --verify VERSION');
    try {
      console.log(assertReleaseVersion(repo, second));
    } catch (error) {
      console.error(error.message);
      process.exit(1);
    }
  } else if (first) setAlphaVersion(first);
  else {
    const tags = execFileSync('git', ['tag', '--list', 'v*-alpha.*'], { cwd: repo, encoding: 'utf8' });
    console.log(nextAlphaVersion(undefined, tags.trim().split('\n').map(tag => tag.slice(1)).filter(Boolean)));
  }
}
