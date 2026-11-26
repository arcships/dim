import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync, readdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';

const [directory, tag, source] = process.argv.slice(2);
const n = '(?:0|[1-9][0-9]*)';
const version = `${n}\\.${n}\\.${n}-nightly\\.${n}`;
const valid = new RegExp(`^${version}$`);
const current = tag.slice(1);
assert(tag.startsWith('v') && valid.test(current) && /^[0-9a-f]{40}$/.test(source));
const compare = (a, b) => {
  const left = a.match(/\d+/g).map(BigInt), right = b.match(/\d+/g).map(BigInt);
  for (let i = 0; i < left.length; i++) if (left[i] !== right[i]) return left[i] > right[i] ? -1 : 1;
  return 0;
};
const archives = ['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64']
  .map(platform => `dimcode-${platform}-${current}.tar.gz`);
assert.deepEqual(readdirSync(directory).sort(), archives.toSorted());
const sums = `SHA256SUMS-${current}`;
writeFileSync(path.join(directory, sums), archives.map(file =>
  `${createHash('sha256').update(readFileSync(path.join(directory, file))).digest('hex')}  ${file}\n`).join(''), { flag: 'wx' });

const gh = (...args) => execFileSync('gh', args, { encoding: 'utf8' }).trim();
const releases = 'repos/{owner}/{repo}/releases';
const previous = gh('api', '--paginate', releases, '--jq', '.[] | select(.tag_name == "Nightly") | @json');
const release = previous && JSON.parse(previous);
if (release) assert(release.prerelease && !release.immutable, 'Nightly must be a mutable prerelease');
else gh('release', 'create', 'Nightly', '--draft', '--prerelease', '--latest=false', '--target', source, '--title', 'Nightly', '--notes', 'Dim nightly builds');

const promoted = release && !release.draft ? release.name.replace(/^Nightly v/, '') : undefined;
if (promoted !== undefined) assert(valid.test(promoted) && release.name === `Nightly v${promoted}`, 'Unexpected Nightly title');
gh('release', 'upload', 'Nightly', ...[...archives, sums].map(file => path.join(directory, file)));
if (!promoted || compare(current, promoted) < 0) {
  gh('release', 'edit', 'Nightly', '--title', `Nightly ${tag}`, '--notes', `Source: ${tag} (${source})`, '--prerelease', '--latest=false', '--draft=false');
}

const id = release?.id || gh('api', `${releases}/tags/Nightly`, '--jq', '.id');
const names = gh('api', '--paginate', `${releases}/${id}/assets`, '--jq', '.[] | select(.state == "uploaded") | .name').split('\n');
const owned = new RegExp(`^(?:SHA256SUMS-(${version})|dimcode-(?:darwin|linux)-(?:arm64|x64)-(${version})\\.tar\\.gz)$`);
const sets = new Map();
for (const name of names) {
  const match = owned.exec(name);
  if (!match) continue;
  const v = match[1] ?? match[2];
  if (!sets.has(v)) sets.set(v, []);
  sets.get(v).push(name);
}
const latest = !promoted || compare(current, promoted) < 0 ? current : promoted;
assert.equal(sets.get(latest)?.length, 5, `Incomplete Nightly asset set for ${latest}`);
for (const [v, files] of sets) {
  if (v !== latest) for (const file of files) gh('release', 'delete-asset', 'Nightly', file, '--yes');
}
