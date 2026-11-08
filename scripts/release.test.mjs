import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {spawn,spawnSync} from 'node:child_process';
import {cpSync,existsSync,mkdirSync,mkdtempSync,readFileSync,readdirSync,renameSync,rmSync,writeFileSync} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {test} from 'node:test';
import {pathToFileURL} from 'node:url';
import {gunzipSync} from 'node:zlib';

const root=path.resolve(import.meta.dirname,'..');
const commit='a'.repeat(40);
const version=JSON.parse(readFileSync(path.join(root,'npm/package.json'))).version;
const digest=(file)=>createHash('sha256').update(readFileSync(file)).digest('hex');
const run=(script,args,env={})=>spawnSync(process.execPath,[path.join(root,'scripts',script),...args],{encoding:'utf8',env:{...process.env,...env}});
function fixture(t) {
  const temporary=mkdtempSync(path.join(os.tmpdir(),'dim release test '));
  t.after(()=>rmSync(temporary,{recursive:true,force:true}));
  return temporary;
}

/** The plugin a logged npm ci ran for; the temporary directory carries the pid. */
const installName=(directory)=>{
  const match=/^(.*)\.partial(?:\.\d+)?$/.exec(path.basename(directory));
  return match?match[1]:path.basename(directory);
};

/** A checkout whose default list names `plugins` and, when given, `optional`, each one carrying a package. */
function packageCheckout(temporary,plugins,optional) {
  const checkout=path.join(temporary,'checkout');
  mkdirSync(path.join(checkout,'scripts'),{recursive:true});
  mkdirSync(path.join(checkout,'runtime'),{recursive:true});
  cpSync(path.join(root,'scripts/stage-plugins.mjs'),path.join(checkout,'scripts/stage-plugins.mjs'));
  writeFileSync(path.join(checkout,'runtime/default-plugins.json'),`${JSON.stringify(optional?{plugins,optional}:{plugins})}\n`);
  for (const [name,relative] of [...Object.entries(plugins),...Object.entries(optional??{})]) {
    const plugin=path.join(checkout,'runtime',relative);
    mkdirSync(plugin,{recursive:true});
    writeFileSync(path.join(plugin,'package.json'),`{"name":"${name}"}\n`);
    writeFileSync(path.join(plugin,'package-lock.json'),`${JSON.stringify({name,lockfileVersion:3,packages:{'':{name}}})}\n`);
    writeFileSync(path.join(plugin,'index.ts'),'export {};\n');
  }
  return path.join(checkout,'scripts/stage-plugins.mjs');
}

/** Temporary directories a stage holds for one plugin. */
const leftovers=(output,relative,kind)=>{
  const parent=path.dirname(path.join(output,relative));
  const name=path.basename(relative).replace(/[.*+?^${}()|[\]\\]/g,'\\$&');
  if(!existsSync(parent)) return [];
  const pattern=new RegExp(`^${name}\\.${kind}(?:\\.\\d+)?$`);
  return readdirSync(parent).filter((entry)=>pattern.test(entry));
};

/** A stub npm on PATH that only records where it ran and creates node_modules. */
function stubNpm(temporary,t) {
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  const log=path.join(temporary,'npm.log');
  writeFileSync(path.join(bin,'npm'),`#!/bin/bash\nprintf '%s\\n' "$PWD" >> "${log}"\nmkdir -p "$PWD/node_modules"\nexit 0\n`,{mode:0o755});
  const previous=process.env.PATH;
  process.env.PATH=`${bin}:${previous}`;
  t.after(()=>{process.env.PATH=previous;});
  return {log,env:{PATH:`${bin}:${previous}`}};
}

/** A child process that stays alive until the test kills it. */
function sleeper(t) {
  const child=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});
  t.after(()=>child.kill('SIGKILL'));
  return child;
}

/** The default plugin payload a release payload must carry; content is not under test here. */
function stubPlugins(temporary,name='plugins-stub') {
  const directory=path.join(temporary,name);
  mkdirSync(directory,{recursive:true});
  writeFileSync(path.join(directory,'default-plugins.json'),'{"plugins":{}}\n');
  return directory;
}

function stubRustNotices(temporary) {
  const file=path.join(temporary,'rust-notices.txt');
  writeFileSync(file,'Rust dependency license fixture');
  return file;
}

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

test('workflow actions use immutable commits',()=>{
  const workflows=path.join(root,'.github/workflows');
  for (const file of readdirSync(workflows).filter(file=>/\.ya?ml(?:\.disabled)?$/.test(file))) {
    const source=readFileSync(path.join(workflows,file),'utf8');
    for (const [,action] of source.matchAll(/^\s*(?:-\s*)?uses:\s*(\S+)/gm)) {
      if (action.startsWith('./')) continue;
      assert.match(action,/^[\w.-]+\/[\w./-]+@[a-f0-9]{40}$/,`${file}: ${action} must use a full commit SHA`);
    }
  }
});

test('the default list stages only its content plugin and runs no npm ci', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  const log=path.join(temporary,'npm.log');
  writeFileSync(path.join(bin,'npm'),`#!/bin/bash\nprintf '%s\\n' "$PWD" >> "${log}"\nmkdir -p "$PWD/node_modules"\nexit 0\n`,{mode:0o755});
  const env={PATH:`${bin}:${process.env.PATH}`};
  const output=path.join(temporary,'staged');
  const first=run('stage-plugins.mjs',[output,'--update'],env);
  assert.equal(first.status,0,first.stderr);
  assert.match(first.stdout,/npm ci: none/);
  assert.equal(existsSync(log),false,'no default plugin needs npm ci');
  const record=JSON.parse(readFileSync(path.join(output,'.stage-plugins.json'),'utf8'));
  assert.equal(record.schemaVersion,1);
  assert.deepEqual(Object.keys(record.plugins),['plugin-authoring']);
  for (const name of ['goal','workflow']) {
    assert.equal(existsSync(path.join(output,'plugins',name)),false,name);
  }
  assert(existsSync(path.join(output,'plugins/plugin-authoring/dim-plugin.json')));
  const defaults=JSON.parse(readFileSync(path.join(output,'default-plugins.json')));
  assert.deepEqual(Object.keys(defaults.plugins),['plugin-authoring']);
});

test('staging a package plugin update is incremental and never leaves a half stage', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  const log=path.join(temporary,'npm.log');
  const npm=(status)=>writeFileSync(path.join(bin,'npm'),`#!/bin/bash\nprintf '%s\\n' "$PWD" >> "${log}"\nmkdir -p "$PWD/node_modules"\nexit ${status}\n`,{mode:0o755});
  npm(0);
  const env={PATH:`${bin}:${process.env.PATH}`};
  const script=packageCheckout(temporary,{first:'plugins/first',second:'plugins/second'});
  const output=path.join(temporary,'staged');
  const record=path.join(output,'.stage-plugins.json');
  const readRecord=()=>JSON.parse(readFileSync(record,'utf8'));
  const installs=()=>existsSync(log)?readFileSync(log,'utf8').trim().split('\n').filter(Boolean):[];
  const update=()=>spawnSync(process.execPath,[script,output,'--update'],{encoding:'utf8',env:{...process.env,...env}});
  const first=update();
  assert.equal(first.status,0,first.stderr);
  assert.match(first.stdout,/npm ci: first, second/);
  assert.deepEqual(installs().map(installName),['first','second']);
  assert.equal(readRecord().schemaVersion,1);
  assert.deepEqual(Object.keys(readRecord().plugins).sort(),['first','second']);
  const second=update();
  assert.equal(second.status,0,second.stderr);
  assert.match(second.stdout,/npm ci: none/);
  assert.equal(installs().length,2);
  const sourceFile=path.join(output,'plugins/first/index.ts');
  const original=readFileSync(sourceFile,'utf8');
  rmSync(sourceFile);
  assert.equal(update().status,0);
  assert.equal(readFileSync(sourceFile,'utf8'),original);
  const changed=readRecord();
  changed.plugins.first.hash='stale';
  writeFileSync(record,`${JSON.stringify(changed)}\n`);
  const reinstalled=update();
  assert.equal(reinstalled.status,0,reinstalled.stderr);
  assert.equal(installs().length,3);
  assert.equal(installName(installs().at(-1)),'first');
  writeFileSync(path.join(temporary,'checkout/runtime/default-plugins.json'),`${JSON.stringify({plugins:{first:'plugins/first'}})}\n`);
  assert.equal(update().status,0);
  assert.equal(existsSync(path.join(output,'plugins/second')),false);
  assert.equal(readRecord().plugins.second,undefined);
  writeFileSync(path.join(output,'plugins/first/marker'),'kept');
  const failing=readRecord();
  failing.plugins.first.hash='stale again';
  writeFileSync(record,`${JSON.stringify(failing)}\n`);
  npm(1);
  const failed=update();
  assert.notEqual(failed.status,0);
  assert.match(failed.stderr,/Cannot stage first/);
  assert.equal(readFileSync(path.join(output,'plugins/first/marker'),'utf8'),'kept');
  assert.deepEqual(leftovers(output,'plugins/first','partial'),[]);
  assert.equal(readRecord().plugins.first.hash,'stale again');
});

test('the record follows the disk when an update fails part-way', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const script=packageCheckout(temporary,{first:'plugins/first',second:'plugins/second'});
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  const log=path.join(temporary,'npm.log');
  const fail=path.join(temporary,'fail-second');
  writeFileSync(path.join(bin,'npm'),`#!/bin/bash\nprintf '%s\\n' "$PWD" >> "${log}"\nmkdir -p "$PWD/node_modules"\ncase "$PWD" in *second*) [ -e "${fail}" ] && exit 1 ;; esac\nexit 0\n`,{mode:0o755});
  const env={PATH:`${bin}:${process.env.PATH}`};
  const output=path.join(temporary,'staged');
  const record=path.join(output,'.stage-plugins.json');
  const readRecord=()=>JSON.parse(readFileSync(record,'utf8'));
  const installs=()=>existsSync(log)?readFileSync(log,'utf8').trim().split('\n').filter(Boolean):[];
  const update=()=>spawnSync(process.execPath,[script,output,'--update'],{encoding:'utf8',env:{...process.env,...env}});
  assert.equal(update().status,0);
  const changed=readRecord();
  changed.plugins.first.hash='stale first';
  changed.plugins.second.hash='stale second';
  writeFileSync(record,`${JSON.stringify(changed)}\n`);
  writeFileSync(fail,'');
  const failed=update();
  assert.notEqual(failed.status,0);
  assert.match(failed.stderr,/Cannot stage second/);
  const afterFailure=readRecord();
  assert.match(afterFailure.plugins.first.hash,/^[0-9a-f]{64}$/);
  assert.equal(afterFailure.plugins.second.hash,`stale second`);
  assert.deepEqual(installs().slice(-2).map(installName),['first','second']);
  rmSync(fail);
  const count=installs().length;
  const fixed=update();
  assert.equal(fixed.status,0,fixed.stderr);
  assert.equal(installs().length,count+1);
  assert.equal(installName(installs().at(-1)),'second');
  const afterFix=readRecord();
  assert.equal(afterFix.plugins.first.hash,afterFailure.plugins.first.hash);
  assert.match(afterFix.plugins.second.hash,/^[0-9a-f]{64}$/);
});

test('a failed swap leaves the live plugin whole until its replacement lands', {skip:process.platform==='win32'}, async (t)=>{
  const temporary=fixture(t);
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  const log=path.join(temporary,'npm.log');
  writeFileSync(path.join(bin,'npm'),`#!/bin/bash\nprintf '%s\\n' "$PWD" >> "${log}"\nmkdir -p "$PWD/node_modules"\nexit 0\n`,{mode:0o755});
  const env={PATH:`${bin}:${process.env.PATH}`};
  const script=packageCheckout(temporary,{first:'plugins/first'});
  const output=path.join(temporary,'staged');
  const first=spawnSync(process.execPath,[script,output,'--update'],{encoding:'utf8',env:{...process.env,...env}});
  assert.equal(first.status,0,first.stderr);
  const live=path.join(output,'plugins/first');
  const modules=path.join(live,'node_modules');
  assert(existsSync(modules));
  const record=path.join(output,'.stage-plugins.json');
  const recordBefore=readFileSync(record,'utf8');
  const source=readFileSync(path.join(live,'index.ts'),'utf8');
  const {updateStage}=await import(pathToFileURL(script).href);
  assert.throws(()=>updateStage(output,{
    rename:(from,to)=>{
      if(to===live&&from.includes('.partial')) throw new Error('injected rename failure');
      renameSync(from,to);
    },
  }),/Cannot stage first/);
  assert(existsSync(modules),'the live dependencies stay in place');
  assert.equal(readFileSync(path.join(live,'index.ts'),'utf8'),source);
  assert.equal(readFileSync(record,'utf8'),recordBefore);
  assert.deepEqual([...leftovers(output,'plugins/first','partial'),...leftovers(output,'plugins/first','previous')],[]);
  const next=spawnSync(process.execPath,[script,output,'--update'],{encoding:'utf8',env:{...process.env,...env}});
  assert.equal(next.status,0,next.stderr);
  assert.match(next.stdout,/npm ci: none/);
});

test('an update recovers the stage a killed run left behind', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  const log=path.join(temporary,'npm.log');
  writeFileSync(path.join(bin,'npm'),`#!/bin/bash\nprintf '%s\\n' "$PWD" >> "${log}"\nmkdir -p "$PWD/node_modules"\nexit 0\n`,{mode:0o755});
  const env={PATH:`${bin}:${process.env.PATH}`};
  const script=packageCheckout(temporary,{first:'plugins/first'});
  const output=path.join(temporary,'staged');
  assert.equal(spawnSync(process.execPath,[script,output,'--update'],{encoding:'utf8',env:{...process.env,...env}}).status,0);
  const live=path.join(output,'plugins/first');
  renameSync(live,`${live}.previous.999999`);
  mkdirSync(`${live}.partial.999999`);
  writeFileSync(path.join(`${live}.partial.999999`,'leftover'),'x');
  const record=path.join(output,'.stage-plugins.json');
  const recordBefore=readFileSync(record,'utf8');
  const installed=readFileSync(log,'utf8').trim().split('\n').filter(Boolean).length;
  const recovered=spawnSync(process.execPath,[script,output,'--update'],{encoding:'utf8',env:{...process.env,...env}});
  assert.equal(recovered.status,0,recovered.stderr);
  assert.match(recovered.stdout,/npm ci: none/);
  assert(existsSync(path.join(live,'index.ts')));
  assert(existsSync(path.join(live,'node_modules')));
  assert.equal(readFileSync(log,'utf8').trim().split('\n').filter(Boolean).length,installed);
  assert.deepEqual([...leftovers(output,'plugins/first','partial'),...leftovers(output,'plugins/first','previous')],[]);
  assert.equal(readFileSync(record,'utf8'),recordBefore);
});

test('two concurrent updates serialize on the stage lock', {skip:process.platform==='win32'}, async (t)=>{
  const temporary=fixture(t);
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  const log=path.join(temporary,'npm.log');
  const ready=path.join(temporary,'ready');
  const release=path.join(temporary,'release');
  writeFileSync(path.join(bin,'npm'),`#!/bin/bash\nprintf '%s\\n' "$PWD" >> "${log}"\nif [ ! -e "${ready}" ]; then\n  : > "${ready}"\n  for i in $(seq 1 1500); do [ -e "${release}" ] && break; sleep 0.02; done\nfi\nmkdir -p "$PWD/node_modules"\nexit 0\n`,{mode:0o755});
  const env={...process.env,PATH:`${bin}:${process.env.PATH}`};
  const output=path.join(temporary,'staged');
  const script=packageCheckout(temporary,{first:'plugins/first'});
  const start=()=>{
    const child=spawn(process.execPath,[script,output,'--update'],{env});
    const result={out:'',err:'',status:null};
    child.stdout.on('data',(piece)=>result.out+=piece);
    child.stderr.on('data',(piece)=>result.err+=piece);
    const done=new Promise((resolve)=>child.on('close',(code)=>{result.status=code;resolve(result);}));
    const timer=setTimeout(()=>child.kill('SIGKILL'),60_000);
    done.then(()=>clearTimeout(timer));
    return {child,result,done};
  };
  const first=start();
  const deadline=Date.now()+30_000;
  while(!existsSync(ready)&&Date.now()<deadline) await new Promise((resolve)=>setTimeout(resolve,20));
  assert(existsSync(ready),'the first run must reach npm ci');
  const second=start();
  const waiting=Date.now()+10_000;
  while(!/Waiting for another staging run/.test(second.result.err)&&Date.now()<waiting) await new Promise((resolve)=>setTimeout(resolve,20));
  writeFileSync(release,'');
  const [firstRun,secondRun]=await Promise.all([first.done,second.done]);
  assert.equal(firstRun.status,0,firstRun.err);
  assert.equal(secondRun.status,0,secondRun.err);
  assert.equal(readFileSync(log,'utf8').trim().split('\n').filter(Boolean).length,1);
  assert.match(secondRun.err,/Waiting for another staging run/);
  assert.match(secondRun.out,/npm ci: none/);
  const record=JSON.parse(readFileSync(path.join(output,'.stage-plugins.json')));
  assert.equal(record.schemaVersion,1);
  assert.deepEqual(Object.keys(record.plugins),['first']);
  assert(existsSync(path.join(output,'plugins/first/node_modules')));
  assert.match(record.plugins.first.hash,/^[0-9a-f]{64}$/);
  assert.deepEqual([...leftovers(output,'plugins/first','partial'),...leftovers(output,'plugins/first','previous')],[]);
});

test('a lock whose owner is gone is taken over', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  writeFileSync(path.join(bin,'npm'),'#!/bin/bash\nmkdir -p "$PWD/node_modules"\nexit 0\n',{mode:0o755});
  const env={PATH:`${bin}:${process.env.PATH}`};
  const output=path.join(temporary,'staged');
  assert.equal(run('stage-plugins.mjs',[output,'--update'],env).status,0);
  const lock=path.join(output,'.stage-plugins.lock');
  mkdirSync(lock);
  const dead=spawnSync(process.execPath,['-e',''],{encoding:'utf8'});
  writeFileSync(path.join(lock,'owner'),`${dead.pid}\n`);
  const taken=run('stage-plugins.mjs',[output,'--update'],env);
  assert.equal(taken.status,0,taken.stderr);
  assert.doesNotMatch(taken.stderr,/Waiting for another staging run/);
  assert.equal(existsSync(lock),false);
});

test('a lock with no owner is taken over without waiting', {skip:process.platform==='win32'}, async (t)=>{
  const temporary=fixture(t);
  stubNpm(temporary,t);
  const output=path.join(temporary,'staged');
  const lock=path.join(output,'.stage-plugins.lock');
  mkdirSync(lock,{recursive:true});
  const {updateStage}=await import(pathToFileURL(path.join(root,'scripts/stage-plugins.mjs')).href);
  updateStage(output,{waitMilliseconds:400,pollMilliseconds:50});
  assert.equal(existsSync(lock),false);
  assert(existsSync(path.join(output,'plugins/plugin-authoring/dim-plugin.json')));
});

test('a lock owned by this process is stale', {skip:process.platform==='win32'}, async (t)=>{
  const temporary=fixture(t);
  stubNpm(temporary,t);
  const output=path.join(temporary,'staged');
  const lock=path.join(output,'.stage-plugins.lock');
  mkdirSync(lock,{recursive:true});
  writeFileSync(path.join(lock,'owner'),`${process.pid}\n`);
  const {updateStage}=await import(pathToFileURL(path.join(root,'scripts/stage-plugins.mjs')).href);
  updateStage(output,{waitMilliseconds:400,pollMilliseconds:50});
  assert.equal(existsSync(lock),false);
});

test('temporaries of a live run survive and are removed once it has exited', {skip:process.platform==='win32'}, async (t)=>{
  const temporary=fixture(t);
  const {env}=stubNpm(temporary,t);
  const output=path.join(temporary,'staged');
  const update=()=>run('stage-plugins.mjs',[output,'--update'],env);
  assert.equal(update().status,0);
  const child=sleeper(t);
  const partial=`${path.join(output,'plugins/plugin-authoring')}.partial.${child.pid}`;
  const recordTemporary=path.join(output,`.stage-plugins.json.tmp.${child.pid}`);
  mkdirSync(partial,{recursive:true});
  writeFileSync(path.join(partial,'live.txt'),'live');
  writeFileSync(recordTemporary,'live');
  assert.equal(update().status,0);
  assert(existsSync(partial),'a live run keeps its partial');
  assert(existsSync(recordTemporary),'a live run keeps its record temporary');
  child.kill('SIGKILL');
  await new Promise((resolve)=>child.on('close',resolve));
  assert.equal(update().status,0);
  assert.equal(existsSync(partial),false);
  assert.equal(existsSync(recordTemporary),false);
});

test('a run whose lock was replaced does not remove the replacement', {skip:process.platform==='win32'}, async (t)=>{
  const temporary=fixture(t);
  stubNpm(temporary,t);
  const output=path.join(temporary,'staged');
  const lock=path.join(output,'.stage-plugins.lock');
  const child=sleeper(t);
  const {updateStage}=await import(pathToFileURL(path.join(root,'scripts/stage-plugins.mjs')).href);
  let replaced=false;
  const rename=(source,target)=>{
    if(!replaced) {
      replaced=true;
      renameSync(lock,path.join(temporary,'replaced.lock'));
      mkdirSync(lock);
      writeFileSync(path.join(lock,'owner'),`${child.pid}\n`);
    }
    renameSync(source,target);
  };
  updateStage(output,{rename});
  assert.equal(replaced,true);
  assert(existsSync(lock),'the replacement lock survives the run');
  assert.equal(readFileSync(path.join(lock,'owner'),'utf8').trim(),String(child.pid));
});

test('a live lock past the wait fails and names it', {skip:process.platform==='win32'}, async (t)=>{
  const temporary=fixture(t);
  const child=sleeper(t);
  const output=path.join(temporary,'staged');
  const lock=path.join(output,'.stage-plugins.lock');
  mkdirSync(lock,{recursive:true});
  writeFileSync(path.join(lock,'owner'),`${child.pid}\n`);
  const {updateStage}=await import(pathToFileURL(path.join(root,'scripts/stage-plugins.mjs')).href);
  assert.throws(()=>updateStage(output,{waitMilliseconds:400,pollMilliseconds:50}),(error)=>{
    assert.match(error.message,/still held/);
    assert(error.message.includes(lock),error.message);
    return true;
  });
  assert(existsSync(lock));
});

test('a re-keyed plugin keeps the directory its new name uses', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const checkout=path.join(temporary,'checkout');
  mkdirSync(path.join(checkout,'scripts'),{recursive:true});
  mkdirSync(path.join(checkout,'runtime/plugins/goal'),{recursive:true});
  cpSync(path.join(root,'scripts/stage-plugins.mjs'),path.join(checkout,'scripts/stage-plugins.mjs'));
  const writeDefaults=(plugins)=>writeFileSync(path.join(checkout,'runtime/default-plugins.json'),`${JSON.stringify({plugins})}\n`);
  writeDefaults({goal:'plugins/goal'});
  writeFileSync(path.join(checkout,'runtime/plugins/goal/package.json'),'{"name":"goal"}\n');
  writeFileSync(path.join(checkout,'runtime/plugins/goal/package-lock.json'),'{"name":"goal","lockfileVersion":3,"packages":{"":{"name":"goal"}}}\n');
  writeFileSync(path.join(checkout,'runtime/plugins/goal/index.ts'),'export {};\n');
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  writeFileSync(path.join(bin,'npm'),'#!/bin/bash\nmkdir -p "$PWD/node_modules"\nexit 0\n',{mode:0o755});
  const env={PATH:`${bin}:${process.env.PATH}`};
  const script=path.join(checkout,'scripts/stage-plugins.mjs');
  const output=path.join(temporary,'staged');
  const update=()=>spawnSync(process.execPath,[script,output,'--update'],{encoding:'utf8',env:{...process.env,...env}});
  assert.equal(update().status,0);
  writeDefaults({'goal-v2':'plugins/goal'});
  const renamed=update();
  assert.equal(renamed.status,0,renamed.stderr);
  assert(existsSync(path.join(output,'plugins/goal/index.ts')));
  const defaults=JSON.parse(readFileSync(path.join(output,'default-plugins.json')));
  assert.deepEqual(defaults.plugins,{'goal-v2':'plugins/goal'});
  const record=JSON.parse(readFileSync(path.join(output,'.stage-plugins.json')));
  assert.deepEqual(Object.keys(record.plugins),['goal-v2']);
});

test('a recorded directory outside the stage is refused before anything changes', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  writeFileSync(path.join(bin,'npm'),'#!/bin/bash\nmkdir -p "$PWD/node_modules"\nexit 0\n',{mode:0o755});
  const env={PATH:`${bin}:${process.env.PATH}`};
  const output=path.join(temporary,'staged');
  assert.equal(run('stage-plugins.mjs',[output,'--update'],env).status,0);
  const record=path.join(output,'.stage-plugins.json');
  const data=JSON.parse(readFileSync(record));
  data.plugins.retired={hash:null,dir:'../outside'};
  writeFileSync(record,`${JSON.stringify(data)}\n`);
  const outside=path.join(temporary,'outside');
  mkdirSync(outside);
  writeFileSync(path.join(outside,'keep.txt'),'kept');
  const before=readFileSync(record,'utf8');
  const refused=run('stage-plugins.mjs',[output,'--update'],env);
  assert.notEqual(refused.status,0);
  assert.match(refused.stderr,/retired/);
  assert.match(refused.stderr,/outside/);
  assert.equal(readFileSync(path.join(outside,'keep.txt'),'utf8'),'kept');
  assert(existsSync(path.join(output,'plugins/plugin-authoring/dim-plugin.json')));
  assert.equal(readFileSync(record,'utf8'),before);
});

test('a recorded stage root is refused before anything changes', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  writeFileSync(path.join(bin,'npm'),'#!/bin/bash\nmkdir -p "$PWD/node_modules"\nexit 0\n',{mode:0o755});
  const env={PATH:`${bin}:${process.env.PATH}`};
  const output=path.join(temporary,'staged');
  assert.equal(run('stage-plugins.mjs',[output,'--update'],env).status,0);
  const record=path.join(output,'.stage-plugins.json');
  const data=JSON.parse(readFileSync(record));
  data.plugins.retired={hash:null,dir:'.'};
  writeFileSync(record,`${JSON.stringify(data)}\n`);
  const before=readFileSync(record,'utf8');
  const refused=run('stage-plugins.mjs',[output,'--update'],env);
  assert.notEqual(refused.status,0);
  assert.match(refused.stderr,/retired/);
  assert.equal(readFileSync(record,'utf8'),before);
  assert(existsSync(path.join(output,'plugins/plugin-authoring/dim-plugin.json')));
});

test('a recorded parent directory does not remove the plugins inside it', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  writeFileSync(path.join(bin,'npm'),'#!/bin/bash\nmkdir -p "$PWD/node_modules"\nexit 0\n',{mode:0o755});
  const env={PATH:`${bin}:${process.env.PATH}`};
  const output=path.join(temporary,'staged');
  assert.equal(run('stage-plugins.mjs',[output,'--update'],env).status,0);
  const record=path.join(output,'.stage-plugins.json');
  const data=JSON.parse(readFileSync(record));
  data.plugins.retired={hash:null,dir:'plugins'};
  writeFileSync(record,`${JSON.stringify(data)}\n`);
  const updated=run('stage-plugins.mjs',[output,'--update'],env);
  assert.equal(updated.status,0,updated.stderr);
  assert(existsSync(path.join(output,'plugins/plugin-authoring/dim-plugin.json')));
  const after=JSON.parse(readFileSync(record));
  assert.equal('retired' in after.plugins,false);
});

test('stages an optional plugin without its dependencies', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const {log,env}=stubNpm(temporary,t);
  const script=packageCheckout(temporary,{first:'plugins/first'},{native:'plugins/native'});
  const source=path.join(temporary,'checkout/runtime/plugins/native');
  writeFileSync(path.join(source,'package.json'),'{"name":"native","dependencies":{"native-lib":"1.0.0"}}\n');
  writeFileSync(path.join(source,'package-lock.json'),`${JSON.stringify({name:'native',lockfileVersion:3,packages:{'':{name:'native',dependencies:{'native-lib':'1.0.0'}},'node_modules/native-lib':{version:'1.0.0'}}})}\n`);
  const output=path.join(temporary,'staged');
  const installs=()=>existsSync(log)?readFileSync(log,'utf8').trim().split('\n').filter(Boolean):[];
  const assertStaged=(label)=>{
    for (const file of ['package.json','package-lock.json','index.ts']) assert(existsSync(path.join(output,'plugins/native',file)),`${label}: ${file}`);
    assert.equal(existsSync(path.join(output,'plugins/native/node_modules')),false,`${label}: the optional plugin has no node_modules`);
    assert(existsSync(path.join(output,'plugins/first/node_modules')),`${label}: the default plugin has its node_modules`);
    assert(installs().length>0,`${label}: the default plugin ran npm ci`);
    assert.deepEqual(installs().filter((directory)=>installName(directory)!=='first'),[],`${label}: npm ci ran only for the default plugin`);
  };
  const fresh=spawnSync(process.execPath,[script,output],{encoding:'utf8',env:{...process.env,...env}});
  assert.equal(fresh.status,0,fresh.stderr);
  assert.match(fresh.stdout,/Staged 2 plugins/);
  assertStaged('fresh stage');
  const updated=spawnSync(process.execPath,[script,output,'--update'],{encoding:'utf8',env:{...process.env,...env}});
  assert.equal(updated.status,0,updated.stderr);
  assert.match(updated.stdout,/Updated 2 plugins/);
  assert.match(updated.stdout,/npm ci: first/);
  assertStaged('update');
});

test('an update removes a node_modules left in an optional plugin', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const {log,env}=stubNpm(temporary,t);
  const script=packageCheckout(temporary,{},{native:'plugins/native'});
  const output=path.join(temporary,'staged');
  const update=()=>spawnSync(process.execPath,[script,output,'--update'],{encoding:'utf8',env:{...process.env,...env}});
  assert.equal(update().status,0);
  const modules=path.join(output,'plugins/native/node_modules');
  mkdirSync(modules,{recursive:true});
  writeFileSync(path.join(modules,'leftover.js'),'');
  const cleaned=update();
  assert.equal(cleaned.status,0,cleaned.stderr);
  assert.equal(existsSync(modules),false,'the leftover node_modules is gone');
  assert(existsSync(path.join(output,'plugins/native/index.ts')));
  assert.equal(existsSync(log),false,'npm ci never ran');
});

test('an update records optional plugins and retires one that leaves the list', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const {env}=stubNpm(temporary,t);
  const script=packageCheckout(temporary,{first:'plugins/first'},{native:'plugins/native'});
  const config=path.join(temporary,'checkout/runtime/default-plugins.json');
  const output=path.join(temporary,'staged');
  const record=path.join(output,'.stage-plugins.json');
  const update=()=>spawnSync(process.execPath,[script,output,'--update'],{encoding:'utf8',env:{...process.env,...env}});
  assert.equal(update().status,0);
  assert.deepEqual(JSON.parse(readFileSync(record,'utf8')).plugins.native,{dir:'plugins/native'});
  writeFileSync(config,`${JSON.stringify({plugins:{first:'plugins/first'}})}\n`);
  const retired=update();
  assert.equal(retired.status,0,retired.stderr);
  assert.equal(existsSync(path.join(output,'plugins/native')),false);
  assert.equal(Object.hasOwn(JSON.parse(readFileSync(record,'utf8')).plugins,'native'),false);
  writeFileSync(record,`${JSON.stringify({schemaVersion:1,plugins:{first:{hash:null,dir:'plugins/first'}}})}\n`);
  const reread=update();
  assert.equal(reread.status,0,reread.stderr);
  assert.match(JSON.parse(readFileSync(record,'utf8')).plugins.first.hash,/^[0-9a-f]{64}$/);
});

test('a portable stage checks default plugin locks but not optional ones', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const {env}=stubNpm(temporary,t);
  const script=packageCheckout(temporary,{first:'plugins/first'},{native:'plugins/native'});
  writeFileSync(path.join(temporary,'checkout/runtime/plugins/native/package-lock.json'),`${JSON.stringify({name:'native',lockfileVersion:3,packages:{'node_modules/native-lib':{version:'1.0.0',os:['linux'],cpu:['x64']}}})}\n`);
  const output=path.join(temporary,'staged');
  const staged=spawnSync(process.execPath,[script,output,'--portable'],{encoding:'utf8',env:{...process.env,...env}});
  assert.equal(staged.status,0,staged.stderr);
  assert(existsSync(path.join(output,'plugins/native/index.ts')));
  assert.equal(existsSync(path.join(output,'plugins/native/node_modules')),false);
});

test('plugins that share a staged directory fail before staging', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const {env}=stubNpm(temporary,t);
  const script=packageCheckout(temporary,{first:'plugins/first'},{alias:'plugins/alias'});
  const runtime=path.join(temporary,'checkout/runtime');
  const config=path.join(runtime,'default-plugins.json');
  const output=path.join(temporary,'staged');
  const record=path.join(output,'.stage-plugins.json');
  const update=()=>spawnSync(process.execPath,[script,output,'--update'],{encoding:'utf8',env:{...process.env,...env}});
  assert.equal(update().status,0);
  const before=readFileSync(record,'utf8');
  const modules=path.join(output,'plugins/first/node_modules');
  assert(existsSync(modules));
  const shared=path.join(runtime,'plugins/shared');
  mkdirSync(shared,{recursive:true});
  writeFileSync(path.join(shared,'package.json'),'{"name":"shared"}\n');
  writeFileSync(path.join(shared,'package-lock.json'),'{"name":"shared","lockfileVersion":3,"packages":{"":{"name":"shared"}}}\n');
  writeFileSync(path.join(shared,'index.ts'),'export {};\n');
  writeFileSync(config,`${JSON.stringify({plugins:{first:'plugins/shared'},optional:{alias:'plugins/./shared'}})}\n`);
  const sharedUpdate=update();
  assert.notEqual(sharedUpdate.status,0);
  assert.match(sharedUpdate.stderr,/first \(plugins\/shared\) and alias \(plugins\/\.\/shared\)/);
  assert.equal(readFileSync(record,'utf8'),before);
  assert(existsSync(modules));
  const fresh=path.join(temporary,'fresh');
  const sharedFresh=spawnSync(process.execPath,[script,fresh],{encoding:'utf8',env:{...process.env,...env}});
  assert.notEqual(sharedFresh.status,0);
  assert.match(sharedFresh.stderr,/first \(plugins\/shared\) and alias \(plugins\/\.\/shared\)/);
  assert.equal(existsSync(fresh),false);
  const nested=path.join(runtime,'plugins/nested');
  mkdirSync(path.join(nested,'inner'),{recursive:true});
  writeFileSync(path.join(nested,'index.ts'),'export {};\n');
  writeFileSync(path.join(nested,'inner/index.ts'),'export {};\n');
  writeFileSync(config,`${JSON.stringify({plugins:{outer:'plugins/nested'},optional:{inner:'plugins/nested/inner'}})}\n`);
  const nestedFresh=path.join(temporary,'nested-fresh');
  const nestedStage=spawnSync(process.execPath,[script,nestedFresh],{encoding:'utf8',env:{...process.env,...env}});
  assert.notEqual(nestedStage.status,0);
  assert.match(nestedStage.stderr,/outer \(plugins\/nested\) and inner \(plugins\/nested\/inner\)/);
  assert.equal(existsSync(nestedFresh),false);
  mkdirSync(path.join(runtime,'plugins/Shared'),{recursive:true});
  writeFileSync(path.join(runtime,'plugins/Shared/index.ts'),'export {};\n');
  writeFileSync(config,`${JSON.stringify({plugins:{upper:'plugins/Shared'},optional:{lower:'plugins/shared'}})}\n`);
  const caseFresh=path.join(temporary,'case-fresh');
  const caseStage=spawnSync(process.execPath,[script,caseFresh],{encoding:'utf8',env:{...process.env,...env}});
  assert.notEqual(caseStage.status,0);
  assert.match(caseStage.stderr,/upper \(plugins\/Shared\) and lower \(plugins\/shared\)/);
  assert.equal(existsSync(caseFresh),false);
  mkdirSync(path.join(runtime,'plugins/shared/inner'),{recursive:true});
  writeFileSync(path.join(runtime,'plugins/shared/inner/index.ts'),'export {};\n');
  writeFileSync(config,`${JSON.stringify({plugins:{upper:'plugins/Shared'},optional:{lower:'plugins/shared/inner'}})}\n`);
  const mixedFresh=path.join(temporary,'mixed-fresh');
  const mixedStage=spawnSync(process.execPath,[script,mixedFresh],{encoding:'utf8',env:{...process.env,...env}});
  assert.notEqual(mixedStage.status,0);
  assert.match(mixedStage.stderr,/upper \(plugins\/Shared\) and lower \(plugins\/shared\/inner\)/);
  assert.equal(existsSync(mixedFresh),false);
});

test('a name listed as both a default and an optional plugin fails before staging', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const {env}=stubNpm(temporary,t);
  const script=packageCheckout(temporary,{first:'plugins/first'},{native:'plugins/native'});
  const config=path.join(temporary,'checkout/runtime/default-plugins.json');
  const output=path.join(temporary,'staged');
  const record=path.join(output,'.stage-plugins.json');
  const update=()=>spawnSync(process.execPath,[script,output,'--update'],{encoding:'utf8',env:{...process.env,...env}});
  assert.equal(update().status,0);
  const before=readFileSync(record,'utf8');
  writeFileSync(config,`${JSON.stringify({plugins:{first:'plugins/first',native:'plugins/native-default'},optional:{native:'plugins/native'}})}\n`);
  const changed=update();
  assert.notEqual(changed.status,0);
  assert.match(changed.stderr,/both "plugins" and "optional"/);
  assert.match(changed.stderr,/native/);
  assert.equal(readFileSync(record,'utf8'),before);
  assert(existsSync(path.join(output,'plugins/native/index.ts')));
  const fresh=path.join(temporary,'fresh');
  const created=spawnSync(process.execPath,[script,fresh],{encoding:'utf8',env:{...process.env,...env}});
  assert.notEqual(created.status,0);
  assert.match(created.stderr,/both "plugins" and "optional"/);
  assert.equal(existsSync(fresh),false);
});

test('a platform-specific plugin package is refused for a portable stage', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const checkout=path.join(temporary,'checkout');
  mkdirSync(path.join(checkout,'scripts'),{recursive:true});
  mkdirSync(path.join(checkout,'runtime/plugins/native'),{recursive:true});
  cpSync(path.join(root,'scripts/stage-plugins.mjs'),path.join(checkout,'scripts/stage-plugins.mjs'));
  writeFileSync(path.join(checkout,'runtime/default-plugins.json'),`${JSON.stringify({plugins:{native:'plugins/native'}})}\n`);
  writeFileSync(path.join(checkout,'runtime/plugins/native/package.json'),'{"name":"native"}\n');
  writeFileSync(path.join(checkout,'runtime/plugins/native/package-lock.json'),`${JSON.stringify({name:'native',lockfileVersion:3,packages:{'node_modules/native-lib':{version:'1.0.0',os:['linux']}}})}\n`);
  writeFileSync(path.join(checkout,'runtime/plugins/native/index.ts'),'export {};\n');
  const output=path.join(temporary,'staged');
  const refused=spawnSync(process.execPath,[path.join(checkout,'scripts/stage-plugins.mjs'),output,'--portable'],{encoding:'utf8'});
  assert.notEqual(refused.status,0);
  assert.match(refused.stderr,/native/);
  assert.match(refused.stderr,/native-lib/);
  assert.equal(existsSync(output),false);
});

test('the current default plugins pass the portable check', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  writeFileSync(path.join(bin,'npm'),'#!/bin/bash\nmkdir -p "$PWD/node_modules"\nexit 0\n',{mode:0o755});
  const output=path.join(temporary,'staged');
  const staged=run('stage-plugins.mjs',[output,'--portable'],{PATH:`${bin}:${process.env.PATH}`});
  assert.equal(staged.status,0,staged.stderr);
  const defaults=JSON.parse(readFileSync(path.join(output,'default-plugins.json')));
  assert.deepEqual(Object.keys(defaults.plugins),['plugin-authoring']);
  assert.deepEqual(readdirSync(path.join(output,'plugins')),['plugin-authoring']);
});

test('stages public native payload, packs it and rejects modified content', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  writeFileSync(path.join(bin,'dimcode'),`#!/bin/sh\nprintf "dimcode ${version}\\n"\n`,{mode:0o755});
  const stage=path.join(temporary,'stage');
  const staged=run('stage-release.mjs',[bin,stage,version,commit,'--plugins',stubPlugins(temporary),'--rust-notices',stubRustNotices(temporary)]);
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
  for (const [archive,prefix] of [[native,`dimcode-${manifest.platform}`],[npm,'package']]) {
    assert.equal(spawnSync('tar',['-xOzf',archive,`${prefix}/licenses/RUST-THIRD-PARTY-NOTICES.txt`],{encoding:'utf8'}).stdout,
      'Rust dependency license fixture');
  }
  assert.equal(spawnSync('tar',['-xOzf',npm,'package/licenses/aimux/LICENSE'],{encoding:'utf8'}).stdout,readFileSync(path.join(root,'vendor/aimux/LICENSE'),'utf8'));
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

test('stages the default plugins beside bin/ in the native archive and the npm package', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  writeFileSync(path.join(bin,'dimcode'),`#!/bin/sh\nprintf "dimcode ${version}\\n"\n`,{mode:0o755});
  const noPlugins=run('stage-release.mjs',[bin,path.join(temporary,'no-plugins'),version,commit]);
  assert.notEqual(noPlugins.status,0);
  assert.match(noPlugins.stderr,/--plugins/);
  assert.equal(existsSync(path.join(temporary,'no-plugins')),false);
  const plugins=path.join(temporary,'plugins');
  const npmBin=path.join(temporary,'npm-bin');
  mkdirSync(npmBin);
  writeFileSync(path.join(npmBin,'npm'),'#!/bin/bash\nmkdir -p "$PWD/node_modules/@arcships/rutis"\nexit 0\n',{mode:0o755});
  const selected=run('stage-plugins.mjs',[plugins],{PATH:`${npmBin}:${process.env.PATH}`});
  assert.equal(selected.status,0,selected.stderr);
  assert.deepEqual(readdirSync(path.join(plugins,'plugins')),['plugin-authoring']);
  assert.notEqual(run('stage-release.mjs',[bin,path.join(temporary,'empty'),version,commit,'--plugins',bin]).status,0);
  const stage=path.join(temporary,'stage');
  assert.notEqual(run('stage-release.mjs',[bin,stage,version,commit,'--plugins',plugins]).status,0);
  assert.equal(existsSync(stage),false);
  const rustNotices=stubRustNotices(temporary);
  writeFileSync(rustNotices,'');
  assert.notEqual(run('stage-release.mjs',[bin,stage,version,commit,'--plugins',plugins,'--rust-notices',rustNotices]).status,0);
  const staged=run('stage-release.mjs',[bin,stage,version,commit,'--plugins',plugins,'--rust-notices',stubRustNotices(temporary)]);
  assert.equal(staged.status,0,staged.stderr);
  const manifest=JSON.parse(readFileSync(path.join(stage,'release-manifest.json')));
  const defaults=JSON.parse(readFileSync(path.join(stage,'default-plugins.json'))).plugins;
  assert.deepEqual(Object.keys(defaults),['plugin-authoring']);
  for (const file of ['default-plugins.json','plugins/plugin-authoring/dim-plugin.json','plugins/plugin-authoring/skills/plugin-authoring/SKILL.md']) assert(manifest.files[file],file);
  const packed=run('pack-release.mjs',[stage,path.join(temporary,'packed')]);
  assert.equal(packed.status,0,packed.stderr);
  const packedManifest=JSON.parse(readFileSync(path.join(temporary,'packed/release-manifest.json')));
  const npm=path.join(temporary,'packed',packedManifest.npmPackages[0].file);
  const native=path.join(temporary,'packed',packedManifest.nativeArchives[0].file);
  assert(entries(npm).includes('package/plugins/plugin-authoring/skills/plugin-authoring/SKILL.md'));
  assert(entries(native).includes(`dimcode-${manifest.platform}/default-plugins.json`));
  for (const archive of [native,npm]) {
    for (const name of ['goal','workflow']) {
      assert.equal(entries(archive).some(file=>file.includes(`/plugins/${name}/`)),false,name);
    }
    assert(entries(archive).some(file=>file.includes('/plugins/plugin-authoring/')));
  }
  const metadata=spawnSync('tar',['-xOzf',npm,'package/package.json'],{encoding:'utf8'});
  assert.deepEqual(JSON.parse(metadata.stdout).files.slice(-2),['default-plugins.json','plugins']);
});

test('stages the Web workspace as one native archive and npm package with its protocol record', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const dist=path.join(temporary,'dist');
  mkdirSync(path.join(dist,'assets'),{recursive:true});
  writeFileSync(path.join(dist,'index.html'),'<!doctype html><title>Dim</title>');
  writeFileSync(path.join(dist,'THIRD-PARTY-NOTICES.txt'),'Third-party license fixture');
  writeFileSync(path.join(dist,'assets/app-1.js'),'export {}');
  const output=path.join(temporary,'web');
  const staged=run('stage-web.mjs',[output,version,commit,'--dist',dist]);
  assert.equal(staged.status,0,staged.stderr);
  const manifest=JSON.parse(readFileSync(path.join(output,'release-manifest.json')));
  assert.equal(manifest.platform,'web');
  assert.equal(manifest.version,version);
  const protocol=Number(readFileSync(path.join(root,'crates/dim-app-server/src/protocol.rs'),'utf8').match(/PROTOCOL_VERSION: u32 = (\d+)/)[1]);
  assert.equal(manifest.protocolVersion,protocol);
  assert.deepEqual(Object.keys(manifest.files).sort(),['LICENSE','NOTICE','web/THIRD-PARTY-NOTICES.txt','web/assets/app-1.js','web/dimcode-web.json','web/index.html']);
  assert.deepEqual(manifest.npmPackages.map(({name,file})=>[name,file]),[['dimcode-web',`dimcode-web-${version}.tgz`]]);
  assert.deepEqual(manifest.nativeArchives.map(({platform,file})=>[platform,file]),[['web',`dimcode-web-${version}.tar.gz`]]);
  const names=entries(path.join(output,`dimcode-web-${version}.tar.gz`)).filter((name)=>!name.endsWith('/')).sort();
  assert.deepEqual(names,['dimcode-web/LICENSE','dimcode-web/NOTICE','dimcode-web/release-manifest.json','dimcode-web/web/THIRD-PARTY-NOTICES.txt','dimcode-web/web/assets/app-1.js','dimcode-web/web/dimcode-web.json','dimcode-web/web/index.html']);
  const record=spawnSync('tar',['-xOzf',path.join(output,`dimcode-web-${version}.tgz`),'package/web/dimcode-web.json'],{encoding:'utf8'});
  assert.equal(record.status,0,record.stderr);
  assert.deepEqual(JSON.parse(record.stdout),{version,protocolVersion:protocol,sourceCommit:commit});
  const metadata=JSON.parse(spawnSync('tar',['-xOzf',path.join(output,`dimcode-web-${version}.tgz`),'package/package.json'],{encoding:'utf8'}).stdout);
  assert.equal(metadata.name,'dimcode-web');
  assert.equal(metadata.license,'SEE LICENSE IN NOTICE');
  assert.equal(metadata.os,undefined);
  const notices=spawnSync('tar',['-xOzf',path.join(output,`dimcode-web-${version}.tgz`),'package/web/THIRD-PARTY-NOTICES.txt'],{encoding:'utf8'});
  assert.equal(notices.stdout,'Third-party license fixture');
  rmSync(path.join(dist,'THIRD-PARTY-NOTICES.txt'));
  assert.notEqual(run('stage-web.mjs',[path.join(temporary,'missing-notices'),version,commit,'--dist',dist]).status,0);
  assert.equal(existsSync(path.join(temporary,'missing-notices')),false);
  rmSync(path.join(dist,'index.html'));
  assert.notEqual(run('stage-web.mjs',[path.join(temporary,'again'),version,commit,'--dist',dist]).status,0);
  assert.equal(existsSync(path.join(temporary,'again')),false);
});

/** An extended attribute is what makes macOS tar add the AppleDouble entry the account package must exclude. */
test('an account build records the account and stages only its executable', {skip:process.platform==='win32'}, (t)=>{
  const temporary=fixture(t);
  const bin=path.join(temporary,'bin');
  mkdirSync(bin);
  writeFileSync(path.join(bin,'dimcode'),`#!/bin/sh\nprintf "dimcode ${version}\\n"\n`,{mode:0o755});
  const stage=path.join(temporary,'stage');
  const staged=run('stage-release.mjs',[bin,stage,version,commit,'--account','--plugins',stubPlugins(temporary),'--rust-notices',stubRustNotices(temporary)]);
  assert.equal(staged.status,0,staged.stderr);
  if(process.platform==='darwin') assert.equal(spawnSync('xattr',['-w','com.apple.provenance','x',path.join(stage,'bin/dimcode')]).status,0,
    'setting the extended attribute must succeed before packing');
  const manifest=JSON.parse(readFileSync(path.join(stage,'release-manifest.json')));
  assert.equal(manifest.account,true);
  assert.deepEqual(Object.keys(manifest.files).sort(),['default-plugins.json','LICENSE','NOTICE','bin/dimcode','licenses/aimux/LICENSE','licenses/RUST-THIRD-PARTY-NOTICES.txt',...['LICENSE','NOTICE','xai-ratatui-inline/NOTICE','xai-ratatui-textarea/NOTICE'].map((file)=>`licenses/grok-build/${file}`)].sort());
  const packed=run('pack-release.mjs',[stage,path.join(temporary,'packed')]);
  assert.equal(packed.status,0,packed.stderr);
  const npmFile=JSON.parse(readFileSync(path.join(temporary,'packed/release-manifest.json'))).npmPackages[0].file;
  assert.deepEqual(entries(path.join(temporary,'packed',npmFile)).filter((name)=>!name.endsWith('/')).sort(),
    ['package/default-plugins.json','package/LICENSE','package/NOTICE','package/bin/dimcode','package/package.json','package/release-manifest.json','package/licenses/aimux/LICENSE','package/licenses/RUST-THIRD-PARTY-NOTICES.txt',...['LICENSE','NOTICE','xai-ratatui-inline/NOTICE','xai-ratatui-textarea/NOTICE'].map((file)=>`package/licenses/grok-build/${file}`)].sort());
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
  const plugins=stubPlugins(temporary);
  const rustNotices=stubRustNotices(temporary);
  const result=run('stage-release.mjs',[bin,stage,version,commit,'--plugins',plugins,'--rust-notices',rustNotices],{DIM_BUILD_PLATFORM:platform});
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
  const mismatch=run('stage-release.mjs',[bin,rejected,version,commit,'--plugins',plugins,'--rust-notices',rustNotices],{DIM_BUILD_PLATFORM:platform});
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
  const dist=path.join(temporary,'dist');
  mkdirSync(dist,{recursive:true});
  writeFileSync(path.join(dist,'index.html'),'<!doctype html>');
  writeFileSync(path.join(dist,'THIRD-PARTY-NOTICES.txt'),'Third-party license fixture');
  const web=run('stage-web.mjs',[path.join(temporary,'release-web'),version,commit,'--dist',dist]);
  assert.equal(web.status,0,web.stderr);
  const complete=path.join(temporary,'complete');
  const collected=run('collect-release.mjs',[temporary,complete]);
  assert.equal(collected.status,0,collected.stderr);
  assert.equal(run('check-native.mjs',[path.join(complete,'native'),version,commit]).status,0);
  const alpha=version.includes('-alpha.');
  const wrapperName=alpha?'dimcode-alpha':'dimcode';
  const wrapper=JSON.parse(spawnSync('tar',['-xOzf',path.join(complete,'npm',`${wrapperName}-${version}.tgz`),'package/package.json'],{encoding:'utf8'}).stdout);
  assert.equal(wrapper.name,wrapperName);
  const readme=spawnSync('tar',['-xOzf',path.join(complete,'npm',`${wrapperName}-${version}.tgz`),'package/README.md'],{encoding:'utf8'});
  assert.equal(readme.stdout,readFileSync(path.join(root,'npm',alpha?'README.alpha.md':'README.md'),'utf8'));
  assert(!entries(path.join(complete,'npm',`${wrapperName}-${version}.tgz`)).includes('package/README.alpha.md'));
  assert.equal(spawnSync('tar',['-xOzf',path.join(complete,'npm',`${wrapperName}-${version}.tgz`),'package/NOTICE'],{encoding:'utf8'}).stdout,readFileSync(path.join(root,'NOTICE'),'utf8'));
  assert.deepEqual(Object.keys(wrapper.bin),alpha?['dima']:['dim','dimcode']);
  assert.equal(wrapper.optionalDependencies['dimcode-web'],version);
  if(process.platform!=='win32') {
    const current=`dimcode-${process.platform}-${process.arch}`;
    const installed=path.join(temporary,'installation');
    mkdirSync(installed);
    const npmCli=path.join(path.dirname(process.execPath),'../lib/node_modules/npm/bin/npm-cli.js');
    const install=spawnSync(process.execPath,[npmCli,'install','--prefix',installed,'--offline','--ignore-scripts','--omit=optional','--no-audit','--no-fund',path.join(complete,'npm',`${current}-${version}.tgz`),path.join(complete,'npm',`dimcode-web-${version}.tgz`),path.join(complete,'npm',`${wrapperName}-${version}.tgz`)],{encoding:'utf8'});
    assert.equal(install.status,0,install.stderr);
    const smoke=spawnSync(process.execPath,[path.join(installed,'node_modules',wrapperName,'bin/dimcode.mjs'),'--version'],{encoding:'utf8'});
    assert.equal(smoke.status,0,smoke.stderr);
    assert.equal(smoke.stdout.trim(),`dimcode ${version}`);
    assert.equal(existsSync(path.join(installed,'node_modules/dimcode-web/web/index.html')),true);
  }
  const incompatible=path.join(temporary,'release-linux-x64/release-manifest.json');
  const manifest=JSON.parse(readFileSync(incompatible));
  manifest.account=true;
  writeFileSync(incompatible,JSON.stringify(manifest));
  assert.notEqual(run('collect-release.mjs',[temporary,path.join(temporary,'inconsistent')]).status,0);
  assert.equal(existsSync(path.join(temporary,'inconsistent')),false);
});
