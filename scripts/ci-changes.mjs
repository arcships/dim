// Decides which PR check groups a change needs. Paths no rule recognizes run
// every group, so a missed rule costs CI time rather than coverage.
import {execFileSync} from 'node:child_process';
import {appendFileSync} from 'node:fs';

export const groups=['rust','windows','xtask','web','scripts','dependencies'];
const everything=Object.fromEntries(groups.map(group=>[group,true]));

const rules=[
  // xtask check validates the plugin pages against the generated snapshots.
  [/^docs\/architecture\/rust-agent\/(plugins|generated)\//,['xtask']],
  // storage_flow reads this document.
  [/^docs\/design\/appserver\/native-storage-format\.md$/,['rust']],
  [/^docs\//,[]],
  [/^deny\.toml$|^LICENSE$/,['dependencies']],
  [/^[^/]+\.md$/,[]],
  // dim-cli's composer test reads the quote cases the Web composer checks.
  [/^web\/src\/features\/chat\/quote-cases\.json$/,['web','rust','windows']],
  [/^web\//,['web']],
  // stage-web.mjs builds the Web workspace.
  [/^scripts\//,['scripts','web']],
  // The Windows job tests dim-cli, which depends only on dim-agent-types in this workspace.
  [/^crates\/(dim-cli|dim-agent-types)\//,['rust','windows']],
  // stage-plugins.mjs packs runtime/plugins/ and its default config file.
  [/^runtime\/(plugins\/|plugins\.default\.json$)/,['rust','scripts']],
  // Rust tests read runtime/ fixtures, and the Node flows exercise npm/ and runtime/.
  [/^(crates|tests|xtask|vendor|runtime|npm)\//,['rust']],
];

export function classify(files) {
  const needed=new Set();
  for (const file of files) {
    const rule=rules.find(([pattern])=>pattern.test(file));
    for (const group of rule?rule[1]:groups) needed.add(group);
  }
  return Object.fromEntries(groups.map(group=>[group,needed.has(group)]));
}

if (import.meta.main) {
  let result=everything;
  // Pushes to main and manual runs verify everything.
  if (process.env.GITHUB_EVENT_NAME==='pull_request') {
    // A pull request checks out its merge commit, whose first parent is the base it merges into.
    // Without rename detection a moved file lists both its old and new paths.
    const files=execFileSync('git',['diff','--name-only','--no-renames','HEAD^1','HEAD'],{encoding:'utf8'}).split('\n').filter(Boolean);
    console.log(files.join('\n'));
    result=classify(files);
  }
  const lines=Object.entries(result).map(([group,value])=>`${group}=${value}`);
  console.log(lines.join('\n'));
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT,lines.join('\n')+'\n');
}
