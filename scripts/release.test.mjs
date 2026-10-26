import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {spawnSync} from 'node:child_process';
import {existsSync,mkdirSync,mkdtempSync,readFileSync,rmSync,writeFileSync} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {test} from 'node:test';
import {gunzipSync} from 'node:zlib';

const root=path.resolve(import.meta.dirname,'..');
const commit='a'.repeat(40);
// The product version the checkout carries; an alpha release commit bumps it.
const version=JSON.parse(readFileSync(path.join(root,'npm/package.json'))).version;
const digest=(file)=>createHash('sha256').update(readFileSync(file)).digest('hex');
const run=(script,args,env={})=>spawnSync(process.execPath,[path.join(root,'scripts',script),...args],{encoding:'utf8',env:{...process.env,...env}});
function fixture(t) {
  const temporary=mkdtempSync(path.join(os.tmpdir(),'dim release test '));
  t.after(()=>rmSync(temporary,{recursive:true,force:true}));
  return temporary;
}

test('account checkout keeps credentials out of arguments and traced logs', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const bin=path.join(temporary,'tools');
  mkdirSync(bin);
  const capture=path.join(temporary,'git-call.json');
  const credential='fixture-user:fixture-release-secret';
  const workflow=readFileSync(path.join(root,'.github/workflows/release.yml'),'utf8');
  const start=workflow.indexOf('          set +x\n');
  const end=workflow.indexOf('          git -C "$source" checkout',start);
  assert(start>=0&&end>start);
  const authenticate=workflow.slice(start,end).replace(/^          /gm,'');
  writeFileSync(path.join(bin,'git'),`#!${process.execPath}
const assert=require('node:assert/strict');
const {writeFileSync}=require('node:fs');
const {spawnSync}=require('node:child_process');
const secret=process.env.DIM_OAUTH_GIT_CREDENTIAL;
const header='Authorization: Basic '+Buffer.from(secret).toString('base64');
const args=process.argv.slice(2);
assert(args.every(arg=>!arg.includes(secret)&&!arg.includes(header)));
const config=spawnSync('/usr/bin/git',['config','--get','http.extraHeader'],{encoding:'utf8'});
assert.equal(config.status,0);
assert.equal(config.stdout.trim()===header,true);
writeFileSync(process.env.TEST_CAPTURE,JSON.stringify(args));
process.exit(Number(process.env.TEST_GIT_EXIT));
`,{mode:0o755});
  const repository='https://example.invalid/account.git';
  const source=path.join(temporary,'account source');
  for(const status of [0,17]) {
    const result=spawnSync('bash',['-c',`set -euxo pipefail\n${authenticate}\ntest -z "\${header+x}"\ntest -z "\${DIM_OAUTH_GIT_CREDENTIAL+x}"`],{
      encoding:'utf8',env:{...process.env,PATH:`${bin}:${process.env.PATH}`,source,
        DIM_OAUTH_REPOSITORY:repository,DIM_OAUTH_GIT_CREDENTIAL:credential,
        TEST_CAPTURE:capture,TEST_GIT_EXIT:String(status)},
    });
    assert.equal(result.status,status,result.stderr);
    assert.deepEqual(JSON.parse(readFileSync(capture)),['clone','--quiet','--no-checkout',repository,source]);
    for(const secret of [credential,Buffer.from(credential).toString('base64')]) {
      assert.equal((result.stdout+result.stderr).includes(secret),false);
    }
  }
});
// Entry names read straight from the gzip'd ustar stream: macOS bsdtar would
// fold AppleDouble `._*` entries back into extended attributes and hide them.
function entries(archive) {
  const data=gunzipSync(readFileSync(archive));
  const names=[];
  for(let offset=0;offset+512<=data.length&&data[offset]!==0;) {
    const field=(start,length)=>data.subarray(offset+start,offset+start+length).toString('utf8').replace(/\0.*$/s,'');
    const size=Number.parseInt(field(124,12).trim()||'0',8);
    const prefix=field(345,155);
    const type=field(156,1);
    if(!['x','g','L'].includes(type)) names.push(prefix?`${prefix}/${field(0,100)}`:field(0,100));
    offset+=512+Math.ceil(size/512)*512;
  }
  return names;
}

test('stages public native payload, packs it and rejects modified content', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  writeFileSync(path.join(bin,'dimcode'),`#!/bin/sh\nprintf "dimcode ${version}\\n"\n`,{mode:0o755});
  const stage=path.join(temporary,'stage');
  const staged=run('stage-release.mjs',[bin,stage,version,commit]);
  assert.equal(staged.status,0,staged.stderr);
  const manifest=JSON.parse(readFileSync(path.join(stage,'release-manifest.json')));
  assert.equal(manifest.account,false);
  assert.equal(existsSync(path.join(stage,'runtime')),false);
  const packed=run('pack-release.mjs',[stage,path.join(temporary,'packed')]);
  assert.equal(packed.status,0,packed.stderr);
  const packedManifest=JSON.parse(readFileSync(path.join(temporary,'packed/release-manifest.json')));
  const metadata=spawnSync('tar',['-xOzf',path.join(temporary,'packed',packedManifest.npmPackages[0].file),'package/package.json'],{encoding:'utf8'});
  assert.equal(JSON.parse(metadata.stdout).license,'SEE LICENSE IN NOTICE');
  const native=path.join(temporary,'packed',packedManifest.nativeArchives[0].file);
  const npm=path.join(temporary,'packed',packedManifest.npmPackages[0].file);
  for (const file of ['LICENSE','NOTICE','xai-ratatui-inline/NOTICE','xai-ratatui-textarea/NOTICE']) {
    const source=readFileSync(path.join(root,'vendor/grok-build',file),'utf8');
    for (const [archive,prefix] of [[native,`dimcode-${manifest.platform}`],[npm,'package']]) {
      const extracted=spawnSync('tar',['-xOzf',archive,`${prefix}/licenses/grok-build/${file}`],{encoding:'utf8'});
      assert.equal(extracted.status,0,extracted.stderr);
      assert.equal(extracted.stdout,source);
    }
  }
  writeFileSync(path.join(stage,'bin/dimcode'),'modified');
  assert.notEqual(run('pack-release.mjs',[stage,path.join(temporary,'tampered')]).status,0);
  assert.equal(existsSync(path.join(temporary,'tampered')),false);
});

test('stages the Web workspace as one native archive and npm package with its protocol record', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const dist=path.join(temporary,'dist');
  mkdirSync(path.join(dist,'assets'),{recursive:true});
  writeFileSync(path.join(dist,'index.html'),'<!doctype html><title>Dim</title>');
  writeFileSync(path.join(dist,'assets/app-1.js'),'export {}');
  const output=path.join(temporary,'web');
  const staged=run('stage-web.mjs',[output,version,commit,'--dist',dist]);
  assert.equal(staged.status,0,staged.stderr);
  const manifest=JSON.parse(readFileSync(path.join(output,'release-manifest.json')));
  assert.equal(manifest.platform,'web');
  assert.equal(manifest.version,version);
  const protocol=Number(readFileSync(path.join(root,'crates/dim-app-server/src/protocol.rs'),'utf8').match(/PROTOCOL_VERSION: u32 = (\d+)/)[1]);
  assert.equal(manifest.protocolVersion,protocol);
  assert.deepEqual(Object.keys(manifest.files).sort(),['LICENSE','web/assets/app-1.js','web/dimcode-web.json','web/index.html']);
  assert.deepEqual(manifest.npmPackages.map(({name,file})=>[name,file]),[['dimcode-web',`dimcode-web-${version}.tgz`]]);
  assert.deepEqual(manifest.nativeArchives.map(({platform,file})=>[platform,file]),[['web',`dimcode-web-${version}.tar.gz`]]);
  const names=entries(path.join(output,`dimcode-web-${version}.tar.gz`)).filter((name)=>!name.endsWith('/')).sort();
  assert.deepEqual(names,['dimcode-web/LICENSE','dimcode-web/release-manifest.json','dimcode-web/web/assets/app-1.js','dimcode-web/web/dimcode-web.json','dimcode-web/web/index.html']);
  const record=spawnSync('tar',['-xOzf',path.join(output,`dimcode-web-${version}.tgz`),'package/web/dimcode-web.json'],{encoding:'utf8'});
  assert.equal(record.status,0,record.stderr);
  assert.deepEqual(JSON.parse(record.stdout),{version,protocolVersion:protocol,sourceCommit:commit});
  const metadata=JSON.parse(spawnSync('tar',['-xOzf',path.join(output,`dimcode-web-${version}.tgz`),'package/package.json'],{encoding:'utf8'}).stdout);
  assert.equal(metadata.name,'dimcode-web');
  assert.equal(metadata.os,undefined);
  // A tree without index.html is not a frontend.
  rmSync(path.join(dist,'index.html'));
  assert.notEqual(run('stage-web.mjs',[path.join(temporary,'again'),version,commit,'--dist',dist]).status,0);
  assert.equal(existsSync(path.join(temporary,'again')),false);
});

test('stages the default plugins as one npm package of JavaScript with pinned dependencies', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const output=path.join(temporary,'plugins');
  const staged=run('stage-plugins.mjs',[output,version,commit]);
  assert.equal(staged.status,0,staged.stderr);
  const manifest=JSON.parse(readFileSync(path.join(output,'release-manifest.json')));
  assert.equal(manifest.platform,'plugins');
  assert.deepEqual(manifest.npmPackages.map(({name,file})=>[name,file]),[['dimcode-plugins',`dimcode-plugins-${version}.tgz`]]);
  assert.deepEqual(manifest.nativeArchives,[]);
  const defaults=JSON.parse(readFileSync(path.join(root,'runtime/plugins.default.json')));
  const files=Object.keys(manifest.files);
  assert(files.includes('plugins.default.json'));
  for(const plugin of defaults.plugins.map((relative)=>relative.slice('./plugins/'.length))) {
    assert(files.includes(`plugins/${plugin}/index.js`),plugin);
    assert(files.includes(`plugins/${plugin}/plugin.json`),plugin);
  }
  // Node does not strip types under node_modules: only JavaScript ships.
  assert.deepEqual(files.filter((file)=>file.endsWith('.ts')),[]);
  const read=(file)=>spawnSync('tar',['-xOzf',path.join(output,`dimcode-plugins-${version}.tgz`),`package/${file}`],{encoding:'utf8'});
  assert.equal(JSON.parse(read('plugins/web/plugin.json').stdout).entry,'index.js');
  assert.doesNotMatch(read('plugins/goal/index.js').stdout,/\.ts['"]/);
  const metadata=JSON.parse(read('package.json').stdout);
  assert.equal(metadata.name,'dimcode-plugins');
  assert.equal(metadata.os,undefined);
  const runtime=JSON.parse(readFileSync(path.join(root,'runtime/package.json')));
  assert.equal(metadata.dependencies['@arcships/rutis-runtime'],runtime.dependencies['@arcships/rutis-runtime']);
  assert(Object.values(metadata.dependencies).every((wanted)=>/^\d+\.\d+\.\d+$/.test(wanted)));
  // Every compiled module parses.
  const unpacked=path.join(temporary,'unpacked');
  mkdirSync(unpacked);
  assert.equal(spawnSync('tar',['-xzf',path.join(output,`dimcode-plugins-${version}.tgz`),'-C',unpacked]).status,0);
  for(const file of files.filter((file)=>file.endsWith('.js'))) {
    const checked=spawnSync(process.execPath,['--check',path.join(unpacked,'package',file)],{encoding:'utf8'});
    assert.equal(checked.status,0,`${file}: ${checked.stderr}`);
  }
  assert.notEqual(run('stage-plugins.mjs',[output,version,commit]).status,0,'an existing destination is refused');
});

test('an account build records the account and stages only its executable', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  writeFileSync(path.join(bin,'dimcode'),`#!/bin/sh\nprintf "dimcode ${version}\\n"\n`,{mode:0o755});
  const stage=path.join(temporary,'stage');
  const staged=run('stage-release.mjs',[bin,stage,version,commit,'--account']);
  assert.equal(staged.status,0,staged.stderr);
  // An extended attribute is what makes macOS tar add an AppleDouble entry.
  if(process.platform==='darwin') assert.equal(spawnSync('xattr',['-w','com.apple.provenance','x',path.join(stage,'bin/dimcode')]).status,0);
  const manifest=JSON.parse(readFileSync(path.join(stage,'release-manifest.json')));
  assert.equal(manifest.account,true);
  assert.deepEqual(Object.keys(manifest.files).sort(),['LICENSE','NOTICE','bin/dimcode',...['LICENSE','NOTICE','xai-ratatui-inline/NOTICE','xai-ratatui-textarea/NOTICE'].map((file)=>`licenses/grok-build/${file}`)].sort());
  const packed=run('pack-release.mjs',[stage,path.join(temporary,'packed')]);
  assert.equal(packed.status,0,packed.stderr);
  const npmFile=JSON.parse(readFileSync(path.join(temporary,'packed/release-manifest.json'))).npmPackages[0].file;
  assert.deepEqual(entries(path.join(temporary,'packed',npmFile)).filter((name)=>!name.endsWith('/')).sort(),
    ['package/LICENSE','package/NOTICE','package/bin/dimcode','package/package.json','package/release-manifest.json',...['LICENSE','NOTICE','xai-ratatui-inline/NOTICE','xai-ratatui-textarea/NOTICE'].map((file)=>`package/licenses/grok-build/${file}`)].sort());
});

test('stages cross-built executables without running them and rejects the wrong architecture', (t)=>{
  const temporary=fixture(t);
  const osName=process.platform==='win32'?'windows':process.platform;
  const cpu=process.arch==='arm64'?'x64':'arm64';
  const platform=`${osName}-${cpu}`;
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  const header=Buffer.alloc(128);
  if(osName==='windows') {
    header.writeUInt16LE(0x5a4d,0);
    header.writeUInt32LE(64,0x3c);
    header.writeUInt32LE(0x00004550,64);
    header.writeUInt16LE(cpu==='arm64'?0xaa64:0x8664,68);
  } else if(osName==='darwin') {
    header.writeUInt32LE(0xfeedfacf,0);
    header.writeUInt32LE(cpu==='arm64'?0x0100000c:0x01000007,4);
  } else {
    Buffer.from('7f454c460201','hex').copy(header);
    header.writeUInt16LE(cpu==='arm64'?183:62,18);
  }
  const suffix=osName==='windows'?'.exe':'';
  const cli=path.join(bin,`dimcode${suffix}`);
  writeFileSync(cli,header,{mode:0o755});
  const stage=path.join(temporary,'stage');
  const result=run('stage-release.mjs',[bin,stage,version,commit],{DIM_BUILD_PLATFORM:platform});
  assert.equal(result.status,0,result.stderr);
  assert.equal(JSON.parse(readFileSync(path.join(stage,'release-manifest.json'))).platform,platform);
  const packed=run('pack-release.mjs',[stage,path.join(temporary,'packed')]);
  assert.equal(packed.status,0,packed.stderr);
  assert.equal(JSON.parse(readFileSync(path.join(temporary,'packed/release-manifest.json'))).nativeArchives[0].platform,platform);
  const rejected=path.join(temporary,'rejected');
  if(osName==='windows') header.writeUInt16LE(cpu==='arm64'?0x8664:0xaa64,68);
  else if(osName==='darwin') header.writeUInt32LE(cpu==='arm64'?0x01000007:0x0100000c,4);
  else header.writeUInt16LE(cpu==='arm64'?62:183,18);
  writeFileSync(cli,header);
  const mismatch=run('stage-release.mjs',[bin,rejected,version,commit],{DIM_BUILD_PLATFORM:platform});
  assert.notEqual(mismatch.status,0);
  assert.match(mismatch.stderr,/architecture mismatch/);
  assert.equal(existsSync(rejected),false);
});

test('collects exact platform packages, installs wrapper offline and rejects inconsistent account provenance', (t)=>{
  const temporary=fixture(t);
  const platforms=['darwin-arm64','darwin-x64','linux-arm64','linux-x64','windows-arm64','windows-x64'];
  for(const platform of platforms) {
    const directory=path.join(temporary,`release-${platform}`);
    const packageRoot=path.join(directory,'scratch/package');
    mkdirSync(path.join(packageRoot,'bin'),{recursive:true});
    const [osName,cpu]=platform.split('-');
    writeFileSync(path.join(packageRoot,'package.json'),JSON.stringify({name:`dimcode-${platform}`,version,repository:{url:'git+https://github.com/arcships/dim.git'},os:[osName==='windows'?'win32':osName],cpu:[cpu]}));
    writeFileSync(path.join(packageRoot,'bin',osName==='windows'?'dimcode.exe':'dimcode'),`#!/bin/sh\nprintf "dimcode ${version}\\n"\n`,{mode:0o755});
    const npmFile=`dimcode-${platform}-${version}.tgz`;
    assert.equal(spawnSync('tar',['-czf',path.join(directory,npmFile),'-C',path.join(directory,'scratch'),'package']).status,0);
    const nativeFile=`dimcode-${platform}-${version}.${osName==='windows'?'zip':'tar.gz'}`;
    writeFileSync(path.join(directory,nativeFile),'native archive fixture');
    writeFileSync(path.join(directory,'release-manifest.json'),JSON.stringify({schemaVersion:1,platform,version,sourceCommit:commit,toolchain:{rust:'1.98.1'},account:false,npmPackages:[{name:`dimcode-${platform}`,file:npmFile,sha256:digest(path.join(directory,npmFile))}],nativeArchives:[{platform,file:nativeFile,sha256:digest(path.join(directory,nativeFile))}]}));
  }
  // The Web workspace is staged once beside the platforms.
  const dist=path.join(temporary,'dist');
  mkdirSync(dist,{recursive:true});
  writeFileSync(path.join(dist,'index.html'),'<!doctype html>');
  const web=run('stage-web.mjs',[path.join(temporary,'release-web'),version,commit,'--dist',dist]);
  assert.equal(web.status,0,web.stderr);
  const plugins=run('stage-plugins.mjs',[path.join(temporary,'release-plugins'),version,commit]);
  assert.equal(plugins.status,0,plugins.stderr);
  const complete=path.join(temporary,'complete');
  const collected=run('collect-release.mjs',[temporary,complete]);
  assert.equal(collected.status,0,collected.stderr);
  assert.equal(run('check-native.mjs',[path.join(complete,'native'),version,commit]).status,0);
  const wrapper=JSON.parse(spawnSync('tar',['-xOzf',path.join(complete,'npm',`dimcode-${version}.tgz`),'package/package.json'],{encoding:'utf8'}).stdout);
  assert.equal(wrapper.optionalDependencies['dimcode-web'],version);
  assert.equal(wrapper.optionalDependencies['dimcode-plugins'],version);
  assert.equal(existsSync(path.join(complete,'npm',`dimcode-plugins-${version}.tgz`)),true);
  if(process.platform!=='win32') {
    const current=`dimcode-${process.platform}-${process.arch}`;
    const installed=path.join(temporary,'installation');
    mkdirSync(installed);
    const npmCli=path.join(path.dirname(process.execPath),'../lib/node_modules/npm/bin/npm-cli.js');
    const install=spawnSync(process.execPath,[npmCli,'install','--prefix',installed,'--offline','--ignore-scripts','--omit=optional','--no-audit','--no-fund',path.join(complete,'npm',`${current}-${version}.tgz`),path.join(complete,'npm',`dimcode-web-${version}.tgz`),path.join(complete,'npm',`dimcode-${version}.tgz`)],{encoding:'utf8'});
    assert.equal(install.status,0,install.stderr);
    const smoke=spawnSync(process.execPath,[path.join(installed,'node_modules/dimcode/bin/dimcode.mjs'),'--version'],{encoding:'utf8'});
    assert.equal(smoke.status,0,smoke.stderr);
    assert.equal(smoke.stdout.trim(),`dimcode ${version}`);
    // The wrapper names the installed frontend to the executable.
    assert.equal(existsSync(path.join(installed,'node_modules/dimcode-web/web/index.html')),true);
  }
  const incompatible=path.join(temporary,'release-linux-x64/release-manifest.json');
  const manifest=JSON.parse(readFileSync(incompatible));
  manifest.account=true;
  writeFileSync(incompatible,JSON.stringify(manifest));
  assert.notEqual(run('collect-release.mjs',[temporary,path.join(temporary,'inconsistent')]).status,0);
  assert.equal(existsSync(path.join(temporary,'inconsistent')),false);
});
