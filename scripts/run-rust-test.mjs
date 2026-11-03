import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {readFileSync} from 'node:fs';
import {dirname} from 'node:path';

const [artifacts,target,...args]=process.argv.slice(2);
assert(artifacts&&target,'Usage: node scripts/run-rust-test.mjs ARTIFACTS TARGET|--workspace [TEST_ARGUMENTS...]');
const workspace=target==='--workspace';
const matches=readFileSync(artifacts,'utf8').split('\n').filter(Boolean).map(line=>JSON.parse(line))
  .filter(record=>record.reason==='compiler-artifact'&&record.profile.test&&record.executable&&(workspace||record.target.name===target));
if(workspace) assert(matches.length,'Expected compiled workspace test targets');
else assert.equal(matches.length,1,`Expected one compiled test target named ${target}`);
let status=0;
await Promise.all(Array.from({length:workspace?2:1},async()=>{
  while(matches.length){
    const record=matches.shift();
    const started=performance.now();
    const child=spawn(record.executable,args,{cwd:dirname(record.manifest_path),stdio:workspace?['ignore','pipe','pipe']:'inherit'});
    let output='';
    if(workspace){
      child.stdout.on('data',chunk=>output+=chunk);
      child.stderr.on('data',chunk=>output+=chunk);
    }
    const [code]=await once(child,'close');
    if(workspace){
      console.log(`::group::${record.target.name} (${((performance.now()-started)/1000).toFixed(2)}s)`);
      process.stdout.write(output);
      console.log('::endgroup::');
    }
    status ||= code??1;
  }
}));
process.exitCode=status;
