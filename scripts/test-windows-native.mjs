import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';

// The Windows-only flows no other platform compiles (#2065). Each test target is
// named explicitly so the selection never runs transport_flow, whose Windows
// branches only switch a backend, and the compiled binary has to hold every
// name: a renamed or removed case fails this step instead of running nothing.
// `flows` and scripts/ci-changes.mjs's `windows_native` paths describe the same
// tests: the paths decide whether this step runs, this list decides what it runs.
// Change both together. If a test here lives outside those paths, a PR touching it
// reaches this step only on the daily sweep or through another matching file; a
// Windows-only test left out of `flows` never runs, the daily sweep included.
export const flows = [
  {package: 'dim-agent-capabilities', target: 'capabilities_flow', tests: [
    'file_storage_flow::file_storage_flow',
    'host_platform_windows_flow::a_helper_without_a_backend_reports_no_confinement_and_keeps_every_limit',
  ]},
  {package: 'dim-app-server', target: 'app_server_flow', tests: [
    'named_pipe_flow::native_client_authenticates_before_rpc_and_exits_without_stdin_eof',
    'named_pipe_flow::named_pipe_is_private_exclusive_and_shares_session_cleanup',
  ]},
];

if (import.meta.main) {
  for (const flow of flows) console.log(`${flow.package} ${flow.target}: ${flow.tests.join(', ')}`);
  if (process.argv.includes('--list')) process.exit(0);
  assert.equal(process.platform, 'win32', 'Native Windows flows require Windows');
  process.chdir(fileURLToPath(new URL('../', import.meta.url)));
  const temporary = mkdtempSync(join(tmpdir(), 'dim-windows-ci-'));
  try {
    const env = {...process.env, DIMCODE_HOME: join(temporary, 'home')};
    for (const flow of flows) {
      const args = ['test', '--locked', '-p', flow.package, '--test', flow.target];
      const listing = execFileSync('cargo', [...args, '--', '--list', '--format=terse'],
        {encoding: 'utf8', env, stdio: ['ignore', 'pipe', 'inherit']});
      const available = listing.split('\n').filter(line => line.endsWith(': test'))
        .map(line => line.slice(0, -': test'.length));
      for (const test of flow.tests) {
        assert.equal(available.filter(name => name === test).length, 1,
          `Expected one ${test} in ${flow.target}`);
      }
      execFileSync('cargo', [...args, '--', '--exact', ...flow.tests], {env, stdio: 'inherit'});
    }
  } finally {
    rmSync(temporary, {recursive: true, force: true});
  }
}
