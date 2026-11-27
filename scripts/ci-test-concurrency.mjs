import {execFileSync} from 'node:child_process';
import {availableParallelism} from 'node:os';

// The workspace test plan: runner hosts may share their physical cores
// between instances and report SMT siblings as CPUs, so the plan scales by
// the cores this job can use instead of its CPU count.
export function plan(platform,cpus,cores,logical){
  if(platform!=='linux') return {jobs:3,threads:3};
  const smt=Math.max(1,Math.floor(logical/Math.max(1,cores)));
  const physical=Math.max(1,Math.floor(cpus/smt));
  return {
    jobs:Math.min(8,Math.max(3,Math.floor(physical/3))),
    threads:Math.min(6,Math.max(2,Math.floor(physical/4))),
  };
}

function topology(args){
  try{
    return execFileSync('lscpu',args,{encoding:'utf8'}).split('\n').filter(line=>line&&!line.startsWith('#'));
  }catch{
    return [];
  }
}

if(import.meta.main){
  const cores=new Set(topology(['-p=Core,Socket'])).size;
  const logical=topology(['-p=CPU']).length;
  const {jobs,threads}=plan(process.platform,availableParallelism(),cores,logical);
  console.log(`jobs=${jobs}`);
  console.log(`threads=${threads}`);
}
