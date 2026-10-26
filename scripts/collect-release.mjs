import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { copyFileSync, cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const [source, destination, ...rest] = process.argv.slice(2);
const usage = 'Usage: node scripts/collect-release.mjs PLATFORM_ARTIFACTS DESTINATION [--platforms P1,P2,...]';
assert(source && destination && (rest.length === 0 || (rest.length === 2 && rest[0] === '--platforms')), usage);
const output = path.resolve(destination);
assert(!existsSync(output), 'Destination already exists');
const repo = path.resolve(import.meta.dirname,'..');
const all = ['darwin-arm64','darwin-x64','linux-arm64','linux-x64','windows-arm64','windows-x64'];
// A subset is for an alpha only; publish-npm.mjs refuses one for beta and stable.
const platforms = rest.length ? rest[1].split(',') : all;
assert(platforms.length > 0 && platforms.every((platform) => all.includes(platform)) && new Set(platforms).size === platforms.length, 'Unknown or repeated platform');
const manifests = platforms.map((platform) => {
  const directory = path.resolve(source,`release-${platform}`);
  const manifest = JSON.parse(readFileSync(path.join(directory,'release-manifest.json')));
  assert.equal(manifest.platform,platform,'Platform mismatch');
  assert.equal(manifest.schemaVersion,1,'Unknown release schema');
  assert.equal(manifest.npmPackages.length,1,'Unexpected package count');
  assert.equal(manifest.nativeArchives.length,1,'Unexpected native archive count');
  const extension = platform.startsWith('windows-') ? 'zip' : 'tar.gz';
  for (const [entry, filename] of [[manifest.npmPackages[0], `dimcode-${platform}-${manifest.version}.tgz`],
    [manifest.nativeArchives[0], `dimcode-${platform}-${manifest.version}.${extension}`]]) {
    assert.equal(entry.file,filename,'Unexpected artifact filename');
    assert.equal(entry.name ?? `dimcode-${entry.platform}`,`dimcode-${platform}`,'Artifact identity mismatch');
    assert(/^[a-f0-9]{64}$/.test(entry.sha256),'Invalid artifact digest');
    const archive = path.join(directory,entry.file);
    assert(lstatSync(archive).isFile(),'Artifact must be a regular file');
    assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'),entry.sha256,'Artifact digest mismatch');
  }
  return {directory,manifest};
});
const first = manifests[0].manifest;
const common = ({version,sourceCommit,toolchain,account}) => ({version,sourceCommit,toolchain,account});
for (const {manifest} of manifests) assert.deepEqual(common(manifest),common(first),'Platform provenance or account mismatch');
// The Web workspace: one artifact for every platform, from stage-web.mjs.
const web = (() => {
  const directory = path.resolve(source,'release-web');
  const manifest = JSON.parse(readFileSync(path.join(directory,'release-manifest.json')));
  assert.equal(manifest.schemaVersion,1,'Unknown web release schema');
  assert.equal(manifest.platform,'web','Web artifact platform mismatch');
  assert.equal(manifest.version,first.version,'Web artifact version mismatch');
  assert.equal(manifest.sourceCommit,first.sourceCommit,'Web artifact source mismatch');
  assert.equal(manifest.npmPackages.length,1,'Unexpected web package count');
  assert.equal(manifest.nativeArchives.length,1,'Unexpected web archive count');
  for (const [entry, filename] of [[manifest.npmPackages[0], `dimcode-web-${manifest.version}.tgz`],
    [manifest.nativeArchives[0], `dimcode-web-${manifest.version}.tar.gz`]]) {
    assert.equal(entry.file,filename,'Unexpected web artifact filename');
    assert(/^[a-f0-9]{64}$/.test(entry.sha256),'Invalid web artifact digest');
    const archive = path.join(directory,entry.file);
    assert(lstatSync(archive).isFile(),'Web artifact must be a regular file');
    assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'),entry.sha256,'Web artifact digest mismatch');
  }
  return {directory,manifest};
})();
// The TypeScript plugins: one platform-independent package, from stage-plugins.mjs.
const plugins = (() => {
  const directory = path.resolve(source,'release-plugins');
  const manifest = JSON.parse(readFileSync(path.join(directory,'release-manifest.json')));
  assert.equal(manifest.schemaVersion,1,'Unknown plugins release schema');
  assert.equal(manifest.platform,'plugins','Plugins artifact platform mismatch');
  assert.equal(manifest.version,first.version,'Plugins artifact version mismatch');
  assert.equal(manifest.sourceCommit,first.sourceCommit,'Plugins artifact source mismatch');
  assert.equal(manifest.npmPackages.length,1,'Unexpected plugins package count');
  assert.equal(manifest.nativeArchives.length,0,'The plugins have no native archive');
  const [entry] = manifest.npmPackages;
  assert.equal(entry.file,`dimcode-plugins-${manifest.version}.tgz`,'Unexpected plugins artifact filename');
  assert(/^[a-f0-9]{64}$/.test(entry.sha256),'Invalid plugins artifact digest');
  const archive = path.join(directory,entry.file);
  assert(lstatSync(archive).isFile(),'Plugins artifact must be a regular file');
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'),entry.sha256,'Plugins artifact digest mismatch');
  return {directory,manifest};
})();
const metadata = JSON.parse(readFileSync(path.join(repo,'npm/package.json')));
assert.equal(metadata.version,first.version,'Wrapper version mismatch');
assert(Object.values(metadata.optionalDependencies).every((version) => version === first.version),'Wrapper dependency version mismatch');
assert.equal(metadata.optionalDependencies['dimcode-web'],first.version,'Wrapper must list dimcode-web');
assert.equal(metadata.optionalDependencies['dimcode-plugins'],first.version,'Wrapper must list dimcode-plugins');
const scratch = mkdtempSync(path.join(os.tmpdir(),'dimcode-collect-'));
try {
  const npm = path.join(output,'npm');
  const native = path.join(output,'native');
  mkdirSync(npm,{recursive:true});
  mkdirSync(native);
  for (const {directory,manifest} of [...manifests, web, plugins]) {
    copyFileSync(path.join(directory,manifest.npmPackages[0].file),path.join(npm,manifest.npmPackages[0].file));
    for (const {file} of manifest.nativeArchives) copyFileSync(path.join(directory,file),path.join(native,file));
  }
  cpSync(path.join(repo,'npm'),path.join(scratch,'package'),{recursive:true});
  if (platforms.length < all.length) {
    // The wrapper references only the platform packages this release ships.
    const wrapper = path.join(scratch,'package/package.json');
    const data = JSON.parse(readFileSync(wrapper));
    data.optionalDependencies = Object.fromEntries([['dimcode-web', first.version], ['dimcode-plugins', first.version], ...platforms.map((platform) => [`dimcode-${platform}`, first.version])]);
    writeFileSync(wrapper,`${JSON.stringify(data,null,2)}\n`);
  }
  copyFileSync(path.join(repo,'LICENSE'),path.join(scratch,'package/LICENSE'));
  const archive = `dimcode-${first.version}.tgz`;
  // COPYFILE_DISABLE keeps macOS tar from adding AppleDouble `._*` entries.
  const packed = spawnSync('tar',['-czf',path.join(npm,archive),'-C',scratch,'package'],{encoding:'utf8',env:{...process.env,COPYFILE_DISABLE:'1'}});
  assert.ifError(packed.error);
  assert.equal(packed.status,0,'Wrapper archive creation failed');
  const result = {schemaVersion:1,...common(first),
    npmPackages:[...manifests.flatMap(({manifest}) => manifest.npmPackages), ...web.manifest.npmPackages, ...plugins.manifest.npmPackages,
      {name:'dimcode',file:archive,sha256:createHash('sha256').update(readFileSync(path.join(npm,archive))).digest('hex')}],
    nativeArchives:[...manifests.flatMap(({manifest}) => manifest.nativeArchives), ...web.manifest.nativeArchives],
  };
  writeFileSync(path.join(npm,'release-manifest.json'),`${JSON.stringify(result,null,2)}\n`);
  copyFileSync(path.join(npm,'release-manifest.json'),path.join(native,'release-manifest.json'));
  writeFileSync(path.join(native,'SHA256SUMS'),result.nativeArchives.map(({sha256,file})=>`${sha256}  ${file}\n`).join(''));
  const checked = spawnSync(process.execPath,[path.join(repo,'scripts/publish-npm.mjs'),npm,first.version,first.sourceCommit],{stdio:'inherit'});
  assert.ifError(checked.error);
  assert.equal(checked.status,0,'Combined npm package validation failed');
  console.log(`Collected ${platforms.length} verified platform artifacts, the Web workspace and the plugins.`);
} catch (error) {
  rmSync(output,{recursive:true,force:true});
  throw error;
} finally {
  rmSync(scratch,{recursive:true,force:true});
}
