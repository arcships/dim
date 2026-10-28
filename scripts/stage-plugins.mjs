import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync } from 'node:fs';
import path from 'node:path';

const [destination, ...extra] = process.argv.slice(2);
assert(destination && extra.length === 0, 'Usage: node scripts/stage-plugins.mjs DESTINATION');
const repo = path.resolve(import.meta.dirname, '..');
const output = path.resolve(destination);
assert(!existsSync(output), 'Destination already exists');
const defaults = path.join(repo, 'runtime/default-plugins.json');
const { plugins } = JSON.parse(readFileSync(defaults, 'utf8'));
const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
const registry = process.env.DIMCODE_NPM_REGISTRY ?? 'https://registry.npmjs.org';
try {
  mkdirSync(output, { recursive: true });
  cpSync(defaults, path.join(output, 'default-plugins.json'));
  for (const [name, relative] of Object.entries(plugins)) {
    const source = path.join(path.dirname(defaults), relative);
    const target = path.join(output, relative);
    const code = existsSync(path.join(source, 'package.json'));
    assert(!code || existsSync(path.join(source, 'package-lock.json')), `${name} has no package-lock.json`);
    cpSync(source, target, {
      recursive: true,
      filter: (file) => {
        const base = path.basename(file);
        return base !== 'node_modules' && base !== 'tsconfig.json' && !base.endsWith('.test.mjs');
      },
    });
    if (!code) continue;
    const result = spawnSync(npm, ['ci', '--omit=dev', '--ignore-scripts', '--no-bin-links',
      '--no-audit', '--no-fund', '--registry', registry], { cwd: target, stdio: 'inherit', shell: process.platform === 'win32' });
    assert.ifError(result.error);
    assert.equal(result.status, 0, `npm ci failed for ${name}`);
  }
  console.log(`Staged ${Object.keys(plugins).length} default plugins.`);
} catch (error) {
  rmSync(output, { recursive: true, force: true });
  throw error;
}
