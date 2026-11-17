import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {mkdirSync,mkdtempSync,rmSync,writeFileSync} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {test} from 'node:test';
import {pathToFileURL} from 'node:url';
import {classify,groups} from './ci-changes.mjs';

const only=(...names)=>Object.fromEntries(groups.map(group=>[group,names.includes(group)]));
const hasGit=spawnSync('git',['--version']).status===0;
const git=(cwd,...args)=>{
  const result=spawnSync('git',['-c','user.name=CI','-c','user.email=ci@example.com',...args],{cwd,encoding:'utf8'});
  assert.equal(result.status,0,result.stderr);
  return result.stdout;
};
const runSelector=(cwd,event,before='')=>{
  const result=spawnSync(process.execPath,[path.join(import.meta.dirname,'ci-changes.mjs')],{
    cwd,encoding:'utf8',env:{...process.env,GITHUB_EVENT_NAME:event,DIM_CI_PUSH_BEFORE:before,GITHUB_OUTPUT:''},
  });
  assert.equal(result.status,0,result.stderr);
  return Object.fromEntries(result.stdout.trim().split('\n').slice(-groups.length).map(line=>{
    const [group,value]=line.split('=');
    return [group,value==='true'];
  }));
};

test('documentation-only changes need no build group',()=>{
  assert.deepEqual(classify(['docs/adr/006-plugin-contract.md','docs/zh/INDEX.md']),only());
  assert.deepEqual(classify(['README.md','AGENTS.md']),only());
  assert.deepEqual(classify(['benchmarks/baselines/result.json']),only());
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
  assert.deepEqual(classify(['docker/Dockerfile']),only('scripts'));
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
  for (const file of ['Cargo.lock','rust-toolchain.toml','.github/workflows/ci.yml','.node-version','dev','skills/x/SKILL.md','benchmarks/runner/main.py'])
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
  assert.deepEqual(runSelector(temporary,'pull_request'),only('rust','windows','node'),'a rename keeps the checks of both paths');
});

test('main push checks use the whole push range in a shallow checkout',{skip:!hasGit},(t)=>{
  const temporary=mkdtempSync(path.join(os.tmpdir(),'dim-ci-push-'));
  t.after(()=>rmSync(temporary,{recursive:true,force:true}));
  const source=path.join(temporary,'source');
  const checkout=path.join(temporary,'checkout');
  git(temporary,'init','-q','-b','main',source);
  writeFileSync(path.join(source,'README.md'),'# Dim\n');
  writeFileSync(path.join(source,'Cargo.toml'),'[workspace]\n');
  git(source,'add','-A');
  git(source,'commit','-q','-m','base');
  const initial=git(source,'rev-parse','HEAD').trim();
  writeFileSync(path.join(source,'Cargo.toml'),'[workspace]\nmembers=[]\n');
  git(source,'commit','-qam','code');
  const docsBase=git(source,'rev-parse','HEAD').trim();
  writeFileSync(path.join(source,'README.md'),'# Dim CLI\n');
  git(source,'commit','-qam','first doc edit');
  writeFileSync(path.join(source,'README.md'),'# Dim coding agent\n');
  git(source,'commit','-qam','second doc edit');
  git(temporary,'clone','-q','--depth=2',pathToFileURL(source).href,checkout);
  assert.notEqual(spawnSync('git',['cat-file','-e',docsBase],{cwd:checkout}).status,0);

  assert.deepEqual(runSelector(checkout,'push',docsBase),only(),'a docs-only range skips all build groups');
  assert.deepEqual(runSelector(checkout,'push',initial),only(...groups),'an earlier code commit keeps the full run');
  assert.deepEqual(runSelector(checkout,'push','f'.repeat(40)),only(...groups),'an unavailable base keeps the full run');
  assert.deepEqual(runSelector(checkout,'workflow_dispatch',docsBase),only(...groups),'manual runs stay full');

  const head=git(checkout,'rev-parse','HEAD').trim();
  const generated=path.join(checkout,'docs/architecture/rust-agent/generated');
  mkdirSync(generated,{recursive:true});
  writeFileSync(path.join(generated,'catalog.md'),'# Catalog\n');
  git(checkout,'add','-A');
  git(checkout,'commit','-q','-m','generated architecture');
  assert.deepEqual(runSelector(checkout,'push',head),only(...groups),'architecture documents keep the full main run');
});
