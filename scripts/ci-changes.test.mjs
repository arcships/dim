import assert from 'node:assert/strict';
import {test} from 'node:test';
import {classify,groups} from './ci-changes.mjs';

const only=(...names)=>Object.fromEntries(groups.map(group=>[group,names.includes(group)]));

test('documentation-only changes skip every check',()=>{
  assert.deepEqual(classify(['docs/adr/006-plugin-contract.md','docs/zh/INDEX.md','README.md','AGENTS.md']),only());
});

test('documents that code reads keep their checks',()=>{
  assert.deepEqual(classify(['docs/architecture/rust-agent/plugins/goal.md']),only('xtask'));
  assert.deepEqual(classify(['docs/design/appserver/native-storage-format.md']),only('rust'));
});

test('dependency policy changes select the scan without compiling the workspace',()=>{
  assert.deepEqual(classify(['deny.toml','LICENSE','docs/release/README.md']),only('dependencies'));
});

test('source areas select their groups',()=>{
  assert.deepEqual(classify(['web/src/main.ts']),only('web'));
  assert.deepEqual(classify(['web/src/features/chat/quote-cases.json']),only('web','rust','windows'));
  assert.deepEqual(classify(['scripts/stage-web.mjs']),only('scripts','web'));
  assert.deepEqual(classify(['crates/dim-app-server/src/lib.rs','runtime/fixtures/session-part.json']),only('rust'));
  assert.deepEqual(classify(['runtime/plugins/web/index.ts']),only('rust','scripts'));
  assert.deepEqual(classify(['crates/dim-cli/src/main.rs']),only('rust','windows'));
  // A file moved from source into docs/ lists both paths.
  assert.deepEqual(classify(['docs/notes.md','crates/dim-cli/src/notes.rs']),only('rust','windows'));
});

test('workspace, toolchain, workflow and unknown paths run everything',()=>{
  for (const file of ['Cargo.lock','rust-toolchain.toml','.github/workflows/ci.yml','.node-version','dev','skills/x/SKILL.md'])
    assert.deepEqual(classify([file]),only(...groups),file);
});
