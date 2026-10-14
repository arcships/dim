import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {lstatSync,readFileSync} from 'node:fs';
import path from 'node:path';
const [directory,version,sourceCommit,...extra]=process.argv.slice(2);
assert(directory&&version&&sourceCommit&&extra.length===0,'Usage: node scripts/check-native.mjs DIRECTORY VERSION SOURCE_COMMIT');
assert(/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$/.test(version),'Invalid version');
assert(/^[a-f0-9]{40}$/.test(sourceCommit),'Invalid source commit');
const manifest=JSON.parse(readFileSync(path.join(directory,'release-manifest.json')));
assert.equal(manifest.version,version,'Release version mismatch');
assert.equal(manifest.sourceCommit,sourceCommit,'Release source mismatch');
const platforms=['darwin-arm64','darwin-x64','linux-arm64','linux-x64','windows-arm64','windows-x64'];
assert.deepEqual(manifest.nativeArchives.map(({platform})=>platform).sort(),platforms,'Missing native platforms');
for (const {platform,file,sha256} of manifest.nativeArchives) {
  assert.equal(file,`dimcode-${platform}-${version}.${platform.startsWith('windows-')?'zip':'tar.gz'}`,'Invalid archive name');
  const archive=path.join(directory,file);
  assert(lstatSync(archive).isFile(),'Native archive must be a regular file');
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'),sha256,'Native archive digest mismatch');
}
console.log(`Validated ${platforms.length} native archives for ${version}.`);
