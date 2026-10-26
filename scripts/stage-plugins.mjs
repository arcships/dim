import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import os from 'node:os';
import path from 'node:path';

// The repository's TypeScript plugins as one platform-independent npm package,
// dimcode-plugins, which the dimcode wrapper lists as an optional dependency
// (docs/design/plugins/PluginManager.md §7). Node does not strip types under
// node_modules, so each plugin ships as JavaScript: its TypeScript with the
// types stripped and its relative `.ts` imports renamed `.js`. The package
// holds the default config file and the plugins; their dependencies and the
// rutis Node runtime are its own pinned dependencies, which npm installs for
// the user's platform (the runtime brings esbuild's native binary).
const [destination, version, sourceCommit, ...rest] = process.argv.slice(2);
const usage = 'Usage: node scripts/stage-plugins.mjs DESTINATION VERSION SOURCE_COMMIT';
assert(destination && version && sourceCommit && rest.length === 0, usage);
assert(/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$/.test(version), 'Invalid version');
assert(/^[a-f0-9]{40}$/.test(sourceCommit), 'Invalid source commit');
const repo = path.resolve(import.meta.dirname, '..');
const productVersion = readFileSync(path.join(repo, 'Cargo.toml'), 'utf8').match(/\[workspace.package\]\s+version = "([^"]+)"/)[1];
assert.equal(version, productVersion, 'Version must match Cargo workspace');
assert.equal(version, JSON.parse(readFileSync(path.join(repo, 'npm/package.json'))).version, 'Version must match npm wrapper');
const output = path.resolve(destination);
assert(!existsSync(output), 'Destination already exists');
const json = (file) => JSON.parse(readFileSync(file, 'utf8'));
const digest = (data) => createHash('sha256').update(data).digest('hex');

// What the plugins' code imports from npm, and the runtime that loads them,
// each at the one version the repository locks.
const runtime = json(path.join(repo, 'runtime/package.json'));
const dependencies = { ...runtime.dependencies };
const defaults = json(path.join(repo, 'runtime/plugins.default.json'));
const plugins = defaults.plugins.map((relative) => {
  assert.match(relative, /^\.\/plugins\/[a-z][a-z0-9-]*$/, `Unexpected default plugin ${relative}`);
  return relative.slice('./plugins/'.length);
});
for (const plugin of plugins) {
  const manifest = path.join(repo, 'runtime/plugins', plugin, 'package.json');
  for (const [name, wanted] of Object.entries(existsSync(manifest) ? json(manifest).dependencies ?? {} : {})) {
    assert(/^\d+\.\d+\.\d+$/.test(wanted), `${plugin} must pin ${name}`);
    assert(dependencies[name] === undefined || dependencies[name] === wanted, `${name}: plugins need one version`);
    dependencies[name] = wanted;
  }
}
// Goal imports the SDK without a package of its own.
dependencies['@arcships/rutis'] ??= runtime.devDependencies['@arcships/rutis'];

/// Relative imports of a `.ts` module, as they become once compiled.
const renamed = (code) => code.replace(/((?:from|import)\s*\(?\s*['"])(\.\.?\/[^'"]+)\.ts(['"])/g, '$1$2.js$3');

const scratch = mkdtempSync(path.join(os.tmpdir(), 'dimcode-plugins-'));
const runTar = (args) => {
  // macOS tar would otherwise add AppleDouble `._*` entries for extended attributes.
  const result = spawnSync('tar', args, { encoding: 'utf8', env: { ...process.env, COPYFILE_DISABLE: '1' } });
  assert.ifError(result.error);
  assert.equal(result.status, 0, 'Archive creation failed');
};
try {
  const name = 'dimcode-plugins';
  const stage = path.join(scratch, 'package');
  for (const plugin of plugins) {
    const source = path.join(repo, 'runtime/plugins', plugin);
    const target = path.join(stage, 'plugins', plugin);
    mkdirSync(target, { recursive: true });
    for (const file of readdirSync(source)) {
      if (file.endsWith('.ts') && !file.endsWith('.d.ts')) {
        const code = stripTypeScriptTypes(readFileSync(path.join(source, file), 'utf8'));
        assert(!code.includes('../../../npm/'), `${plugin}/${file} imports the SDK declarations at run time`);
        writeFileSync(path.join(target, file.replace(/\.ts$/, '.js')), renamed(code));
      } else if (file === 'plugin.json') {
        const row = json(path.join(source, file));
        row.entry = row.entry.replace(/\.ts$/, '.js');
        writeFileSync(path.join(target, file), `${JSON.stringify(row, null, 2)}\n`);
      } else if (file === 'package.json') {
        // Its code may read it (Web's user agent); it names no entry to load.
        copyFileSync(path.join(source, file), path.join(target, file));
      }
    }
  }
  writeFileSync(path.join(stage, 'plugins.default.json'), `${JSON.stringify(defaults, null, 2)}\n`);
  copyFileSync(path.join(repo, 'LICENSE'), path.join(stage, 'LICENSE'));
  const files = {};
  for (const entry of readdirSync(stage, { withFileTypes: true, recursive: true })) {
    assert(entry.isFile() || entry.isDirectory(), 'Unexpected plugin file type');
    if (entry.isFile()) files[path.relative(stage, path.join(entry.parentPath, entry.name)).split(path.sep).join('/')] = digest(readFileSync(path.join(entry.parentPath, entry.name)));
  }
  const toolchain = { node: process.versions.node };
  const staged = { schemaVersion: 1, version, sourceCommit, platform: 'plugins', toolchain, files };
  writeFileSync(path.join(stage, 'release-manifest.json'), `${JSON.stringify(staged, null, 2)}\n`);
  writeFileSync(path.join(stage, 'package.json'), `${JSON.stringify({
    name, version, description: "Dim's TypeScript plugins, loaded by default by the dimcode App Server", license: 'MIT',
    repository: { type: 'git', url: 'git+https://github.com/arcships/dim.git' },
    type: 'module',
    files: ['plugins', 'plugins.default.json', 'LICENSE', 'release-manifest.json'],
    dependencies: Object.fromEntries(Object.entries(dependencies).sort()),
  }, null, 2)}\n`);
  mkdirSync(output, { recursive: true });
  const archive = `${name}-${version}.tgz`;
  runTar(['-czf', path.join(output, archive), '-C', scratch, 'package']);
  const result = { ...staged,
    npmPackages: [{ name, platform: 'plugins', file: archive, sha256: digest(readFileSync(path.join(output, archive))) }],
    nativeArchives: [],
  };
  writeFileSync(path.join(output, 'release-manifest.json'), `${JSON.stringify(result, null, 2)}\n`);
  console.log(`Staged ${plugins.length} plugins ${version}.`);
} catch (error) {
  rmSync(output, { recursive: true, force: true });
  throw error;
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
