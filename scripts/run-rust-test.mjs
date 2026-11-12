import assert from 'node:assert/strict';
import {spawn,spawnSync} from 'node:child_process';
import {once} from 'node:events';
import {readFileSync} from 'node:fs';
import {dirname} from 'node:path';

const [artifacts,target,...args]=process.argv.slice(2);
assert(artifacts&&target,'Usage: node scripts/run-rust-test.mjs ARTIFACTS TARGET|--workspace [TEST_ARGUMENTS...]');
const workspace=target==='--workspace';
const timeout=Number(process.env.DIM_TEST_TIMEOUT_MS??600_000);
assert(Number.isSafeInteger(timeout)&&timeout>0,'DIM_TEST_TIMEOUT_MS must be a positive integer');
const matches=readFileSync(artifacts,'utf8').split('\n').filter(Boolean).map(line=>JSON.parse(line))
  .filter(record=>record.reason==='compiler-artifact'&&record.profile.test&&record.executable&&(workspace||record.target.name===target));
if(workspace) assert(matches.length,'Expected compiled workspace test targets');
else assert.equal(matches.length,1,`Expected one compiled test target named ${target}`);
let status=0;
let stopping=false;
const active=new Set();
function terminate(child){
  if(!child.pid) return;
  if(process.platform==='win32'){
    spawnSync('taskkill',['/PID',String(child.pid),'/T','/F'],{stdio:'ignore'});
  }else{
    const pids=[-child.pid];
    if(child.exitCode===null&&child.signalCode===null){
      const listed=spawnSync('ps',['-A','-o','pid=,ppid='],{encoding:'utf8'});
      if(listed.status===0){
        const processes=listed.stdout.trim().split('\n').map(line=>line.trim().split(/\s+/).map(Number));
        const descendants=[child.pid];
        for(const parent of descendants){
          for(const [pid,ppid] of processes) if(ppid===parent) descendants.push(pid);
        }
        pids.unshift(...descendants.slice(1).reverse());
      }else console.error(`[test] Cannot list descendants of ${child.pid}: ${listed.error?.message??listed.stderr}`);
    }
    for(const pid of pids){
      try{process.kill(pid,'SIGKILL');}
      catch(error){if(error.code!=='ESRCH') throw error;}
    }
  }
}
for(const [signal,code] of [['SIGINT',130],['SIGTERM',143]]){
  process.once(signal,()=>{
    stopping=true;
    status=code;
    for(const child of active) terminate(child);
  });
}
await Promise.all(Array.from({length:workspace?2:1},async()=>{
  while(matches.length&&!stopping){
    const record=matches.shift();
    const started=performance.now();
    const label=`${record.target.name} (${dirname(record.manifest_path)})`;
    console.error(`[test] START ${label}`);
    const child=spawn(record.executable,args,{cwd:dirname(record.manifest_path),stdio:workspace?['ignore','inherit','inherit']:'inherit',detached:process.platform!=='win32'});
    active.add(child);
    let timedOut=false;
    const timer=setTimeout(()=>{
      timedOut=true;
      console.error(`[test] TIMEOUT ${label} after ${timeout}ms`);
      terminate(child);
    },timeout);
    try{
      const [code,signal]=await once(child,'close');
      const result=timedOut?124:code??1;
      console.error(`[test] END ${label}: exit=${result}, signal=${signal??'none'}, ${(performance.now()-started).toFixed(0)}ms`);
      status ||= result;
    }catch(error){
      console.error(`[test] ERROR ${label}: ${error.message}`);
      status ||= 1;
    }finally{
      clearTimeout(timer);
      terminate(child);
      active.delete(child);
    }
  }
}));
process.exitCode=status;
