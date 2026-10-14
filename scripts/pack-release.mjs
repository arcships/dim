import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { copyFileSync, cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync,
  readdirSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const [source, destination, ...extra] = process.argv.slice(2);
assert(source && destination && extra.length === 0, 'Usage: node scripts/pack-release.mjs STAGED_RELEASE DESTINATION');
const input = path.resolve(source);
const output = path.resolve(destination);
assert(!existsSync(output), 'Destination already exists');
const manifest = JSON.parse(readFileSync(path.join(input, 'release-manifest.json')));
assert.equal(manifest.schemaVersion, 1, 'Unknown release schema');
const [osName, cpu] = manifest.platform.split('-');
assert(['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'windows-arm64', 'windows-x64'].includes(manifest.platform), 'Unknown platform');
assert(/^[a-f0-9]{40}$/.test(manifest.sourceCommit), 'Invalid source commit');
assert(/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$/.test(manifest.version), 'Invalid version');
const digest = (data) => createHash('sha256').update(data).digest('hex');
const files = {};
const inventory = (directory) => {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const absolute = path.join(directory, entry.name);
    const relative = path.relative(input, absolute).split(path.sep).join('/');
    if (entry.isDirectory()) inventory(absolute);
    else {
      assert(entry.isFile(), 'Release links and special files are forbidden');
      if (relative !== 'release-manifest.json') files[relative] = digest(readFileSync(absolute));
    }
  }
};
inventory(input);
assert.deepEqual(files, manifest.files, 'Staged release content changed');
const suffix = osName === 'windows' ? '.exe' : '';
assert(files[`bin/dimcode${suffix}`] && files[`bin/dim-app-server${suffix}`], 'Missing executables');
const name = `dimcode-${manifest.platform}`;
const scratch = mkdtempSync(path.join(os.tmpdir(), 'dimcode-pack-'));
const runTar = (args) => {
  const result = spawnSync('tar', args, {encoding:'utf8'});
  assert.ifError(result.error);
  assert.equal(result.status, 0, 'Archive creation failed');
};
try {
  mkdirSync(output, {recursive:true});
  const native = `${name}-${manifest.version}.${osName === 'windows' ? 'zip' : 'tar.gz'}`;
  cpSync(input, path.join(scratch, name), {recursive:true});
  runTar(osName === 'windows' ? ['-a', '-cf', path.join(output, native), '-C', scratch, name]
    : ['-czf', path.join(output, native), '-C', scratch, name]);
  const packageRoot = path.join(scratch, 'package');
  cpSync(input, packageRoot, {recursive:true});
  writeFileSync(path.join(packageRoot, 'package.json'), `${JSON.stringify({
    name, version:manifest.version, description:'Dim native CLI and App Server', license:manifest.plugin ? 'SEE LICENSE IN NOTICE' : 'MIT',
    repository:{type:'git',url:'git+https://github.com/arcships/dim.git'},
    os:[osName === 'windows' ? 'win32' : osName],cpu:[cpu],files:['bin','runtime','plugins','LICENSE','NOTICE','release-manifest.json'],
  },null,2)}\n`);
  const archive = `${name}-${manifest.version}.tgz`;
  runTar(['-czf', path.join(output, archive), '-C', scratch, 'package']);
  const result = {...manifest,
    npmPackages:[{name,file:archive,sha256:digest(readFileSync(path.join(output, archive)))}],
    nativeArchives:[{platform:manifest.platform,file:native,sha256:digest(readFileSync(path.join(output, native)))}],
  };
  writeFileSync(path.join(output, 'release-manifest.json'), `${JSON.stringify(result,null,2)}\n`);
  console.log(`Packed ${manifest.platform} ${manifest.version}.`);
} catch (error) {
  rmSync(output,{recursive:true,force:true});
  throw error;
} finally {
  rmSync(scratch,{recursive:true,force:true});
}
