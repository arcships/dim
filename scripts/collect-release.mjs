import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { copyFileSync, cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const [source, destination, ...extra] = process.argv.slice(2);
assert(source && destination && extra.length === 0, 'Usage: node scripts/collect-release.mjs PLATFORM_ARTIFACTS DESTINATION');
const output = path.resolve(destination);
assert(!existsSync(output), 'Destination already exists');
const repo = path.resolve(import.meta.dirname,'..');
const platforms = ['darwin-arm64','darwin-x64','linux-arm64','linux-x64','windows-arm64','windows-x64'];
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
const common = ({version,sourceCommit,toolchain,plugin}) => ({version,sourceCommit,toolchain,plugin});
for (const {manifest} of manifests) assert.deepEqual(common(manifest),common(first),'Platform provenance or plugin mismatch');
const metadata = JSON.parse(readFileSync(path.join(repo,'npm/package.json')));
assert.equal(metadata.version,first.version,'Wrapper version mismatch');
assert(Object.values(metadata.optionalDependencies).every((version) => version === first.version),'Wrapper dependency version mismatch');
const scratch = mkdtempSync(path.join(os.tmpdir(),'dimcode-collect-'));
try {
  const npm = path.join(output,'npm');
  const native = path.join(output,'native');
  mkdirSync(npm,{recursive:true});
  mkdirSync(native);
  for (const {directory,manifest} of manifests) {
    copyFileSync(path.join(directory,manifest.npmPackages[0].file),path.join(npm,manifest.npmPackages[0].file));
    copyFileSync(path.join(directory,manifest.nativeArchives[0].file),path.join(native,manifest.nativeArchives[0].file));
  }
  cpSync(path.join(repo,'npm'),path.join(scratch,'package'),{recursive:true});
  copyFileSync(path.join(repo,'LICENSE'),path.join(scratch,'package/LICENSE'));
  const archive = `dimcode-${first.version}.tgz`;
  const packed = spawnSync('tar',['-czf',path.join(npm,archive),'-C',scratch,'package'],{encoding:'utf8'});
  assert.ifError(packed.error);
  assert.equal(packed.status,0,'Wrapper archive creation failed');
  const result = {schemaVersion:1,...common(first),
    npmPackages:[...manifests.flatMap(({manifest}) => manifest.npmPackages),
      {name:'dimcode',file:archive,sha256:createHash('sha256').update(readFileSync(path.join(npm,archive))).digest('hex')}],
    nativeArchives:manifests.flatMap(({manifest}) => manifest.nativeArchives),
  };
  writeFileSync(path.join(npm,'release-manifest.json'),`${JSON.stringify(result,null,2)}\n`);
  copyFileSync(path.join(npm,'release-manifest.json'),path.join(native,'release-manifest.json'));
  writeFileSync(path.join(native,'SHA256SUMS'),result.nativeArchives.map(({sha256,file})=>`${sha256}  ${file}\n`).join(''));
  const checked = spawnSync(process.execPath,[path.join(repo,'scripts/publish-npm.mjs'),npm,first.version,first.sourceCommit],{stdio:'inherit'});
  assert.ifError(checked.error);
  assert.equal(checked.status,0,'Combined npm package validation failed');
  console.log(`Collected ${platforms.length} verified platform artifacts.`);
} catch (error) {
  rmSync(output,{recursive:true,force:true});
  throw error;
} finally {
  rmSync(scratch,{recursive:true,force:true});
}
