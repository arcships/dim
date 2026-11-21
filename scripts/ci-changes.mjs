import {execFileSync,spawnSync} from 'node:child_process';
import {appendFileSync} from 'node:fs';

export const groups=['rust','windows','static','xtask','node','eval','execution','web','scripts','macos_scripts','dependencies'];
const everything=Object.fromEntries(groups.map(group=>[group,true]));
const documentation=/^docs\/|^[^/]+\.md$/;
const cargoManifest=/(?:^|\/)Cargo\.toml$/;

const rules=[
  [/^benchmarks\/baselines\/[^/]+\.json$/,[]],
  [/^\.agents\/|^skills\/|^\.github\/(?:ISSUE_TEMPLATE\/|pull_request_template\.md$|PULL_REQUEST_TEMPLATE\.md$)/,[]],
  [/^docs\/architecture\/rust-agent\/generated\/catalog\.md$/,['xtask']],
  [/^docs\/architecture\/rust-agent\/(plugins|generated)\//,['static']],
  [/^docs\/design\/appserver\/native-storage-format\.md$/,['rust']],
  // scripts/tui-tmux.test.mjs reads this page and checks every scenario is documented.
  [/^docs\/architecture\/tui-e2e\.md$/,['scripts']],
  // crates/dim-event-hub tests compare their strings to its appendix blocks.
  [/^docs\/design\/plugins\/EventHub\.md$/,['rust']],
  // runtime/plugin-sdk.test.mjs compiles its fenced TypeScript blocks.
  [/^docs\/typescript-plugin-authoring\.md$/,['rust','node']],
  // The ADR check runs in the changes job; docs alone need no build group.
  [documentation,[]],
  [/^deny\.toml$|^LICENSE$/,['dependencies']],
  [/^web\/src\/features\/chat\/quote-cases\.json$/,['web','rust','windows']],
  [/^web\/src\/.*\.(ts|vue|mjs|js)$/,['web','static']],
  [/^web\//,['web']],
  [/^scripts\/run-rust-test\.mjs$/,groups],
  [/^scripts\/test-macos-native(?:\.test)?\.mjs$/,['rust','scripts']],
  [/^scripts\/stage-web\.mjs$/,['scripts','web']],
  [/^scripts\/(code-layout\/|code-layout-darwin\.test\.mjs$)/,['scripts','macos_scripts']],
  [/^scripts\/(stage-release|pack-release|native-binary|store-formats|release\.test)\.mjs$/,['scripts','macos_scripts']],
  [/^scripts\//,['scripts']],
  [/^docker\//,['scripts']],
  [/^crates\/dim-cli\//,['rust','windows']],
  [/^crates\/dim-client\//,['rust','windows','node']],
  [/^crates\/dim-agent-types\//,['rust','windows','node']],
  [/^crates\/dimcode\//,['rust']],
  [/^crates\/dim-app-server\/src\/protocol\.rs$/,['rust','node','web','scripts']],
  [/^crates\/dim-app-server\/src\/storage\/sqlite\.rs$/,['rust','node','scripts','macos_scripts']],
  [/^crates\/dim-app-server\/resources\//,['rust','node','scripts']],
  [/^crates\/dim-agent-capabilities\/src\/(background_exec|workspace_tools\/exec|host_platform)/,['rust','node','execution']],
  [/^tests\/integration\/tests\/(local_execution|local_support|minimal_execution)/,['rust','node','execution']],
  [/^vendor\/grok-build\//,['rust','windows','node']],
  [/^xtask\/check\//,['static','xtask']],
  [/^xtask\//,['rust','node','eval']],
  [/^(crates|tests|vendor|runtime|npm)\//,['rust','node']],
];

export function classify(files) {
  const needed=new Set();
  for (const file of files) {
    const rule=rules.find(([pattern])=>pattern.test(file));
    for (const group of rule?rule[1]:groups) needed.add(group);
    if (cargoManifest.test(file)) needed.add('dependencies');
  }
  return Object.fromEntries(groups.map(group=>[group,needed.has(group)]));
}

function changedFiles(base) {
  return execFileSync('git',['diff','--name-only','--no-renames','-z',base,'HEAD','--'],{encoding:'utf8'}).split('\0').filter(Boolean);
}

if (import.meta.main) {
  let result=everything;
  if (process.env.GITHUB_EVENT_NAME==='pull_request') {
    const files=changedFiles('HEAD^1');
    console.log(JSON.stringify(files));
    result=classify(files);
  } else if (process.env.GITHUB_EVENT_NAME==='push') {
    const base=process.env.DIM_CI_PUSH_BEFORE;
    if (/^[a-f0-9]{40}$/.test(base) && base!=='0'.repeat(40)) {
      try {
        if (spawnSync('git',['cat-file','-e',`${base}^{commit}`]).status!==0) {
          execFileSync('git',['fetch','--no-tags','--depth=1','origin',base],{stdio:'pipe',timeout:30_000});
        }
        const files=changedFiles(base);
        if (files.length) {
          console.log(JSON.stringify(files));
          const selected=classify(files);
          // Stage ADR 026 D5 rollout: skip main pushes that select no check
          // group, and keep code pushes full until the daily sweep is verified.
          if (!Object.values(selected).includes(true)) result=selected;
        }
      } catch {
        console.log('Could not compare the push base; running all check groups.');
      }
    }
  }
  const lines=Object.entries(result).map(([group,value])=>`${group}=${value}`);
  console.log(lines.join('\n'));
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT,lines.join('\n')+'\n');
}
