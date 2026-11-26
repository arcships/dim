import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, readlinkSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';

test('rolling nightly installs only complete releases and retains only the latest version', { skip: process.platform === 'win32' }, t => {
  const root = mkdtempSync(path.join(tmpdir(), 'dim-nightly-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const file = (...parts) => path.join(root, ...parts);
  const env = { ...process.env, HOME: root, XDG_DATA_HOME: file('data'), RELEASE_FIXTURE: root, PATH: `${root}:${process.env.PATH}` };
  const assets = file('assets');
  mkdirSync(assets);
  mkdirSync(file('payload/bin'), { recursive: true });
  writeFileSync(file('gh'), `#!/bin/sh
set -eu
case "$1 $2" in
  'api --paginate') case "$3" in */assets) ls "$HOME/assets";; */releases) if [ -f "$HOME/release" ]; then cat "$HOME/release"; fi;; *) exit 2;; esac;;
  'api repos/{owner}/{repo}/releases/tags/Nightly') echo 1;;
  'release create') test "$3" = Nightly && echo '{"id":1,"prerelease":true,"draft":true,"tag_name":"Nightly"}' >"$HOME/release";;
  'release upload') test "$3" = Nightly; shift 3; for f do cp "$f" "$HOME/assets/"; if [ -f "$HOME/interrupt" ]; then exit 1; fi; done;;
  'release edit') test "$3" = Nightly && jq --arg name "$5" '.name=$name | .draft=false' "$HOME/release" >"$HOME/next" && mv "$HOME/next" "$HOME/release";;
  'release delete-asset') test "$3" = Nightly && rm "$HOME/assets/$4";;
  *) exit 2;;
esac\n`, { mode: 0o755 });
  writeFileSync(file('curl'), `#!/bin/sh
set -eu
case "$2" in
  https://api.github.com/repos/arcships/dim/releases/tags/Nightly)
    if [ -f "$RELEASE_FIXTURE/stale-release" ]; then
      mv "$RELEASE_FIXTURE/stale-release" "$4"
    else
      cp "$RELEASE_FIXTURE/release" "$4"
    fi;;
  https://github.com/arcships/dim/releases/download/Nightly/*)
    asset=\${2##*/}
    if [ "$asset" = SHA256SUMS-1.2.3-nightly.9 ] && [ -f "$RELEASE_FIXTURE/stale-sums" ]; then
      mv "$RELEASE_FIXTURE/stale-sums" "$4"
    elif [ -f "$RELEASE_FIXTURE/assets/$asset" ]; then
      cp "$RELEASE_FIXTURE/assets/$asset" "$4"
    else
      printf 404
      exit 22
    fi
    if [ "\${5:-}" = -w ]; then printf 200; fi;;
  *) exit 2;;
esac\n`, { mode: 0o755 });
  const platforms = ['linux-arm64', 'linux-x64', 'darwin-arm64', 'darwin-x64'];
  function publish(n, ok = true) {
    const v = `1.2.3-nightly.${n}`, dir = file(v);
    mkdirSync(dir);
    writeFileSync(file('payload/bin/dimcode'), `#!/bin/sh\necho 'dimcode ${v}'\n`, { mode: 0o755 });
    for (const p of platforms) execFileSync('tar', ['-czf', path.join(dir, `dimcode-${p}-${v}.tar.gz`), '-C', file('payload'), '.']);
    const run = spawnSync(process.execPath, [path.join(import.meta.dirname, 'publish-nightly.mjs'), dir, `v${v}`, 'a'.repeat(40)], { env, encoding: 'utf8' });
    assert.equal(run.status === 0, ok, run.stderr);
  }
  const install = (home = root) => spawnSync('sh', [path.join(import.meta.dirname, 'install-nightly.sh')], {
    env: { ...env, HOME: home, XDG_DATA_HOME: path.join(home, 'data') }, encoding: 'utf8', timeout: 5000,
  });
  const installed = (command = 'dim-nightly') => execFileSync(file('.local/bin', command), ['--version'], { env, encoding: 'utf8' }).trim();
  const shortcut = file('.local/bin/dimn');
  const release = () => JSON.parse(readFileSync(file('release'))).name;
  publish(8);
  assert.equal(install().status, 0);
  assert.equal(installed(), 'dimcode 1.2.3-nightly.8');
  assert.equal(installed('dimn'), 'dimcode 1.2.3-nightly.8');
  rmSync(shortcut);
  assert.equal(install().status, 0);
  assert.equal(installed('dimn'), 'dimcode 1.2.3-nightly.8');
  writeFileSync(file('.local/bin/dima'), 'npm installation');
  writeFileSync(file('assets/unrelated.txt'), 'keep');
  writeFileSync(file('interrupt'), '');
  publish(12, false);
  assert.equal(release(), 'Nightly v1.2.3-nightly.8');
  assert.equal(install(file('fresh-after-interruption')).status, 0);
  rmSync(file('interrupt'));
  publish(9);
  const oldRelease = readFileSync(file('release'));
  const oldSums = readFileSync(file('assets/SHA256SUMS-1.2.3-nightly.9'));
  publish(11);
  publish(10);
  assert.equal(release(), 'Nightly v1.2.3-nightly.11');
  const expected = [...platforms.map(p => `dimcode-${p}-1.2.3-nightly.11.tar.gz`), 'SHA256SUMS-1.2.3-nightly.11'];
  assert.deepEqual(readdirSync(assets).sort(), [...expected, 'unrelated.txt'].sort());
  const tar = file('assets', `dimcode-${process.platform}-${process.arch}-1.2.3-nightly.11.tar.gz`);
  const bytes = readFileSync(tar);
  writeFileSync(tar, 'corrupted');
  assert.match(install().stderr, /Checksum mismatch/);
  assert.equal(installed(), 'dimcode 1.2.3-nightly.8');
  writeFileSync(tar, bytes);
  for (const missing of ['checksum', 'archive']) {
    writeFileSync(file('stale-release'), oldRelease);
    if (missing === 'archive') writeFileSync(file('stale-sums'), oldSums);
    const home = file(`promotion-before-${missing}`);
    const run = install(home);
    assert.equal(run.status, 0, `Expected recovery when promotion removes the ${missing}: ${run.stderr}`);
    assert.equal(execFileSync(path.join(home, '.local/bin/dim-nightly'), ['--version'], { encoding: 'utf8' }).trim(), 'dimcode 1.2.3-nightly.11');
  }
  rmSync(tar);
  assert.match(install().stderr, /Could not download Nightly assets.*404/);
  assert.equal(installed(), 'dimcode 1.2.3-nightly.8');
  writeFileSync(tar, bytes);
  for (let i = 0; i < 2; i++) {
    const run = install();
    assert.equal(run.status, 0, run.stderr);
    assert.equal(installed(), 'dimcode 1.2.3-nightly.11');
    assert.equal(installed('dimn'), 'dimcode 1.2.3-nightly.11');
  }
  rmSync(shortcut);
  symlinkSync('unrelated-command', shortcut);
  const conflict = install();
  assert.equal(conflict.status, 0, conflict.stderr);
  assert.match(conflict.stderr, /Keeping existing .*dimn/);
  assert.equal(readlinkSync(shortcut), 'unrelated-command');
  assert.equal(installed(), 'dimcode 1.2.3-nightly.11');
  rmSync(shortcut);
  writeFileSync(file('ln'), `#!/bin/sh
set -eu
case "$*" in *dimn*)
  if [ ! -e "$HOME/.local/bin/dimn" ]; then
    if [ -f "$HOME/race-directory" ]; then mkdir "$HOME/.local/bin/dimn"; else echo foreign >"$HOME/.local/bin/dimn"; fi
  fi;;
esac
exec /bin/ln "$@"\n`, { mode: 0o755 });
  for (const directory of [false, true]) {
    if (directory) writeFileSync(file('race-directory'), '');
    const collision = install();
    assert.equal(collision.status, 0, collision.stderr);
    assert.match(collision.stderr, /Keeping existing .*dimn/);
    if (directory) assert.deepEqual(readdirSync(shortcut), []);
    else assert.equal(readFileSync(shortcut, 'utf8'), 'foreign\n');
    rmSync(shortcut, { recursive: true });
  }
  assert.equal(installed(), 'dimcode 1.2.3-nightly.11');
  assert.equal(readFileSync(file('.local/bin/dima'), 'utf8'), 'npm installation');
});
