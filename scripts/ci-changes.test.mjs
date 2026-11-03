import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {mkdirSync,mkdtempSync,rmSync,writeFileSync} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {test} from 'node:test';
import {classify,groups} from './ci-changes.mjs';

const only=(...names)=>Object.fromEntries(groups.map(group=>[group,names.includes(group)]));
const hasGit=spawnSync('git',['--version']).status===0;
const git=(cwd,...args)=>{
  const result=spawnSync('git',['-c','user.name=CI','-c','user.email=ci@example.com',...args],{cwd,encoding:'utf8'});
  assert.equal(result.status,0,result.stderr);
  return result.stdout;
};

test('documentation-only changes need no build group',()=>{
  assert.deepEqual(classify(['docs/adr/006-plugin-contract.md','docs/zh/INDEX.md']),only());
  assert.deepEqual(classify(['README.md','AGENTS.md']),only());
});

test('documents that code reads keep their checks',()=>{
  assert.deepEqual(classify(['docs/architecture/rust-agent/plugins/goal.md']),only('xtask'));
  assert.deepEqual(classify(['docs/design/appserver/native-storage-format.md']),only('rust'));
});

test('dependency policy changes select the scan without compiling the workspace',()=>{
  assert.deepEqual(classify(['deny.toml','LICENSE']),only('dependencies'));
});

test('source areas select their groups',()=>{
  assert.deepEqual(classify(['web/src/main.ts']),only('web'));
  assert.deepEqual(classify(['web/src/features/chat/quote-cases.json']),only('web','rust','windows'));
  assert.deepEqual(classify(['scripts/stage-web.mjs']),only('scripts','web'));
  assert.deepEqual(classify(['crates/dim-app-server/src/lib.rs','runtime/fixtures/session-part.json']),only('rust','node'));
  assert.deepEqual(classify(['crates/dim-cli/src/main.rs']),only('rust','windows'));
  assert.deepEqual(classify(['docs/notes.md','crates/dim-cli/src/notes.rs']),only('rust','windows'));
});

test('bridge, evaluation and wall-clock flows follow their owning paths',()=>{
  assert.deepEqual(classify(['crates/dim-cli/src/ui/tui.rs']),only('rust','windows'));
  assert.deepEqual(classify(['crates/dimcode/tests/cli_flow/models.rs']),only('rust'));
  assert.deepEqual(classify(['crates/dim-agent-types/src/tool.rs']),only('rust','windows','node'));
  assert.deepEqual(classify(['xtask/src/scenario_eval.rs']),only('rust','node','eval'));
  assert.deepEqual(classify(['xtask/scenario-eval/case.json']),only('rust','node','eval'));
  assert.deepEqual(classify(['crates/dim-agent-capabilities/src/workspace_tools/exec.rs']),only('rust','node','execution'));
  assert.deepEqual(classify(['crates/dim-agent-capabilities/src/host_platform/mod.rs']),only('rust','node','execution'));
  assert.deepEqual(classify(['tests/integration/tests/local_execution.rs']),only('rust','node','execution'));
  assert.deepEqual(classify(['scripts/run-rust-test.mjs']),only(...groups));
});

test('workspace, toolchain, workflow and unknown paths run everything',()=>{
  for (const file of ['Cargo.lock','rust-toolchain.toml','.github/workflows/ci.yml','.node-version','dev','skills/x/SKILL.md'])
    assert.deepEqual(classify([file]),only(...groups),file);
});

/** A merge commit's first parent is the base; --no-renames keeps a moved file's old path in the diff. */
test('a renamed file keeps the checks of its old path',{skip:!hasGit},(t)=>{
  const temporary=mkdtempSync(path.join(os.tmpdir(),'dim-ci-changes-'));
  t.after(()=>rmSync(temporary,{recursive:true,force:true}));
  mkdirSync(path.join(temporary,'crates/dim-cli'),{recursive:true});
  mkdirSync(path.join(temporary,'runtime'));
  writeFileSync(path.join(temporary,'crates/dim-cli/old.rs'),'pub fn old() {}\n');
  git(temporary,'init','-q');
  git(temporary,'checkout','-q','-b','main');
  git(temporary,'add','-A');
  git(temporary,'commit','-q','-m','base');
  git(temporary,'checkout','-q','-b','rename');
  git(temporary,'mv','crates/dim-cli/old.rs','runtime/old.rs');
  git(temporary,'commit','-q','-m','rename');
  git(temporary,'checkout','-q','main');
  git(temporary,'merge','-q','--no-ff','-m','merge','rename');
  const result=spawnSync(process.execPath,[path.join(import.meta.dirname,'ci-changes.mjs')],{
    cwd:temporary,encoding:'utf8',env:{...process.env,GITHUB_EVENT_NAME:'pull_request',GITHUB_OUTPUT:''},
  });
  assert.equal(result.status,0,result.stderr);
  const output=Object.fromEntries(result.stdout.trim().split('\n').slice(-groups.length).map(line=>{
    const [group,value]=line.split('=');
    return [group,value==='true'];
  }));
  assert.deepEqual(output,only('rust','windows','node'),'a rename keeps the checks of both paths');
});
