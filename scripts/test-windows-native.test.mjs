import assert from 'node:assert/strict';
import {test} from 'node:test';
import {flows} from './test-windows-native.mjs';

test('the Windows-native selection names two targets and their flows', () => {
  assert.deepEqual(flows.map(flow => `${flow.package}/${flow.target}`),
    ['dim-agent-capabilities/capabilities_flow', 'dim-app-server/app_server_flow']);
  assert(!flows.some(flow => flow.target === 'transport_flow'), 'the selection never runs transport_flow');
  for (const flow of flows) {
    assert(flow.tests.length, `${flow.target} names the flows it runs`);
    for (const name of flow.tests) assert.match(name, /^\w+::\w+$/, `${name} names one test in a module`);
  }
});
