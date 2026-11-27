import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { isAccepted, parseRefs, sameRepository, stripNonLinks } from './issue-gate.mjs';

const ROOT = fileURLToPath(new URL('..', import.meta.url));
const REPOSITORY = 'arcships/dim';

function reply(status, body) {
  return { status, body };
}

async function startFakeApi(handler) {
  const requests = [];
  const server = http.createServer((request, response) => {
    let body = '';
    request.on('data', (chunk) => {
      body += chunk;
    });
    request.on('end', () => {
      const url = new URL(request.url, 'http://127.0.0.1');
      const record = { method: request.method, path: url.pathname, search: url.search, body };
      requests.push(record);
      let result;
      try {
        result = handler(record);
      } catch (error) {
        result = reply(500, { message: error.message });
      }
      if (!result) result = reply(404, { message: 'not found' });
      response.writeHead(result.status, { 'Content-Type': 'application/json' });
      response.end(result.status === 204 ? undefined : JSON.stringify(result.body));
    });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return {
    requests,
    url: `http://127.0.0.1:${server.address().port}`,
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}

function runGate(api, { eventName, event, target, targetHead, discover = false }) {
  return new Promise((resolve, reject) => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'dim-issue-gate-'));
    const eventPath = path.join(directory, 'event.json');
    fs.writeFileSync(eventPath, JSON.stringify(event));
    const env = {
      ...process.env,
      GITHUB_API_URL: api.url,
      GITHUB_TOKEN: 'test-token',
      GITHUB_REPOSITORY: REPOSITORY,
      GITHUB_EVENT_NAME: eventName,
      GITHUB_EVENT_PATH: eventPath,
      GITHUB_SERVER_URL: 'https://github.com',
      GITHUB_RUN_ID: '12345',
      ISSUE_GATE_RETRY_ATTEMPTS: '1',
      ISSUE_GATE_RETRY_DELAY_MS: '0',
    };
    if (target !== undefined) {
      env.TARGET_PR = String(target);
      env.TARGET_HEAD = targetHead ?? 'current-head';
    }
    const child = spawn(process.execPath, ['scripts/issue-gate.mjs', ...(discover ? ['--discover'] : [])], {
      cwd: ROOT,
      env,
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => {
      stdout += chunk;
    });
    child.stderr.on('data', (chunk) => {
      stderr += chunk;
    });
    const timer = setTimeout(() => {
      child.kill();
      reject(new Error('gate script timed out'));
    }, 30_000);
    child.on('error', (error) => {
      clearTimeout(timer);
      fs.rmSync(directory, { recursive: true, force: true });
      reject(error);
    });
    child.on('close', (status) => {
      clearTimeout(timer);
      fs.rmSync(directory, { recursive: true, force: true });
      resolve({ status, stdout, stderr });
    });
  });
}

function closingRefs(nodes) {
  return reply(200, { data: { repository: { pullRequest: { closingIssuesReferences: { nodes } } } } });
}

function pullRequest(body = 'Fixes #1', head = 'current-head') {
  return { number: 77, body, head: { sha: head }, state: 'open' };
}

function issue(labels = ['P1']) {
  return { number: 1, labels: labels.map((name) => ({ name })) };
}

test('parses closing and reference keywords', () => {
  assert.deepEqual(parseRefs('Fixes #1'), [1]);
  assert.deepEqual(parseRefs('Closes #2'), [2]);
  assert.deepEqual(parseRefs('Resolves #3'), [3]);
  assert.deepEqual(parseRefs('Refs #4'), [4]);
  assert.deepEqual(parseRefs('fixes: #5'), [5]);
});

test('collects multiple references on one keyword line', () => {
  assert.deepEqual(parseRefs('Fixes #1257 and #2272'), [1257, 2272]);
  assert.deepEqual(parseRefs('Fixes #1, #2, #2'), [1, 2]);
});

test('ignores text that is not a link', () => {
  assert.deepEqual(parseRefs('prefixes #1'), []);
  assert.deepEqual(parseRefs('see #1'), []);
  assert.deepEqual(parseRefs('Fixes #1\nRefs #2'), [1, 2]);
  assert.deepEqual(parseRefs('Fixes owner/repo#1'), []);
  assert.deepEqual(parseRefs('Fixes #1 and owner/repo#2'), [1]);
});

test('ignores fenced code, inline code, comments and quotes', () => {
  assert.deepEqual(parseRefs('```\nFixes #1\n```'), []);
  assert.deepEqual(parseRefs('~~~\nFixes #1\n~~~'), []);
  assert.deepEqual(parseRefs('`Fixes #1`'), []);
  assert.deepEqual(parseRefs('<!-- Fixes #1 -->'), []);
  assert.deepEqual(parseRefs('<!--\nFixes #1\n-->'), []);
  assert.deepEqual(parseRefs('> Fixes #1'), []);
});

test('keep a real link after stripping non-link contexts', () => {
  const body = '```\nFixes #1\n```\n\nRefs #2272';
  assert.deepEqual(parseRefs(body), [2272]);
  assert.ok(stripNonLinks(body).includes('Refs #2272'));
});

test('caps the number of parsed references', () => {
  const refs = Array.from({ length: 25 }, (_, index) => `#${index + 1}`).join(' ');
  assert.deepEqual(parseRefs(`Fixes ${refs}`), Array.from({ length: 20 }, (_, index) => index + 1));
});

test('only same-repository timeline sources are gated', () => {
  const base = { issue: { repository_url: 'https://api.github.com/repos/arcships/dim' } };
  const foreign = { issue: { repository_url: 'https://api.github.com/repos/other/dim' } };
  assert.equal(sameRepository(base, 'arcships/dim'), true);
  assert.equal(sameRepository(foreign, 'arcships/dim'), false);
  assert.equal(sameRepository(undefined, 'arcships/dim'), false);
});

test('accepted means P0, P1 or P2', () => {
  assert.equal(isAccepted(['P0']), true);
  assert.equal(isAccepted(['P1', 'design']), true);
  assert.equal(isAccepted(['P2', 'P3']), true);
  assert.equal(isAccepted(['P3']), false);
  assert.equal(isAccepted(['design']), false);
  assert.equal(isAccepted([]), false);
});

test('evaluates the current PR instead of the event payload', async (t) => {
  const checks = [];
  const patches = [];
  const api = await startFakeApi((request) => {
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/pulls/77`) {
      return reply(200, pullRequest());
    }
    if (request.method === 'POST' && request.path === '/graphql') {
      return closingRefs([]);
    }
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/issues/1`) {
      return reply(200, issue());
    }
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/commits/current-head/check-runs`) {
      return reply(200, { check_runs: checks });
    }
    if (request.method === 'POST' && request.path === `/repos/${REPOSITORY}/check-runs`) {
      const created = { id: 1, name: 'issue-gate', external_id: 'issue-gate:77' };
      checks.push(created);
      return reply(201, created);
    }
    if (request.method === 'PATCH' && request.path === `/repos/${REPOSITORY}/check-runs/1`) {
      patches.push(JSON.parse(request.body));
      return reply(200, {});
    }
    return undefined;
  });
  t.after(() => api.close());

  const event = {
    pull_request: { number: 77, body: 'Fixes #2', head: { sha: 'event-head' }, state: 'open' },
  };
  const result = await runGate(api, { eventName: 'pull_request_target', event, target: 77 });

  assert.equal(result.status, 0, result.stderr);
  assert.ok(api.requests.some((request) => request.path === `/repos/${REPOSITORY}/pulls/77`));
  assert.ok(!api.requests.some((request) => request.path.includes('event-head')));
  const completed = patches.filter((patch) => patch.status === 'completed');
  assert.equal(completed.length, 1);
  assert.equal(completed[0].conclusion, 'success');
  assert.equal(completed[0].output.title, 'Linked accepted issue');
});

test('reconciles a duplicate check after a transient lookup failure', async (t) => {
  const runs = [{ id: 1, name: 'issue-gate', external_id: 'issue-gate:77' }];
  const patches = [];
  let listCalls = 0;
  const api = await startFakeApi((request) => {
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/pulls/77`) {
      return reply(200, pullRequest());
    }
    if (request.method === 'POST' && request.path === '/graphql') {
      return closingRefs([]);
    }
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/issues/1`) {
      return reply(200, issue());
    }
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/commits/current-head/check-runs`) {
      listCalls += 1;
      if (listCalls === 1) return reply(500, { message: 'temporary failure' });
      return reply(200, { check_runs: runs });
    }
    if (request.method === 'POST' && request.path === `/repos/${REPOSITORY}/check-runs`) {
      const created = { id: 2, name: 'issue-gate', external_id: 'issue-gate:77' };
      runs.push(created);
      return reply(201, created);
    }
    if (request.method === 'PATCH' && request.path.startsWith(`/repos/${REPOSITORY}/check-runs/`)) {
      patches.push({
        id: Number(request.path.split('/').pop()),
        ...JSON.parse(request.body),
      });
      return reply(200, {});
    }
    return undefined;
  });
  t.after(() => api.close());

  const result = await runGate(api, {
    eventName: 'pull_request_target',
    event: { pull_request: { number: 77 } },
    target: 77,
  });

  assert.equal(result.status, 0, result.stderr);
  const completed = patches.filter((patch) => patch.status === 'completed');
  assert.deepEqual(completed.map((patch) => patch.id).sort(), [1, 2]);
  assert.ok(completed.every((patch) => patch.conclusion === 'success'));
});

test('completes the check as a failure when evaluation cannot read the issue', async (t) => {
  const checks = [];
  const patches = [];
  const api = await startFakeApi((request) => {
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/pulls/77`) {
      return reply(200, pullRequest());
    }
    if (request.method === 'POST' && request.path === '/graphql') {
      return closingRefs([]);
    }
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/issues/1`) {
      return reply(500, { message: 'temporary failure' });
    }
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/commits/current-head/check-runs`) {
      return reply(200, { check_runs: checks });
    }
    if (request.method === 'POST' && request.path === `/repos/${REPOSITORY}/check-runs`) {
      const created = { id: 1, name: 'issue-gate', external_id: 'issue-gate:77' };
      checks.push(created);
      return reply(201, created);
    }
    if (request.method === 'PATCH' && request.path === `/repos/${REPOSITORY}/check-runs/1`) {
      patches.push(JSON.parse(request.body));
      return reply(200, {});
    }
    return undefined;
  });
  t.after(() => api.close());

  const result = await runGate(api, {
    eventName: 'pull_request_target',
    event: { pull_request: { number: 77 } },
    target: 77,
  });

  assert.equal(result.status, 1);
  const completed = patches.filter((patch) => patch.status === 'completed');
  assert.equal(completed.length, 1);
  assert.equal(completed[0].conclusion, 'failure');
  assert.equal(completed[0].output.title, 'Could not read linked issues');
});

test('fails discovery when the open PR list cannot be read', async (t) => {
  const api = await startFakeApi((request) => {
    if (request.path === `/repos/${REPOSITORY}/pulls`) {
      return reply(500, { message: 'temporary failure' });
    }
    return undefined;
  });
  t.after(() => api.close());

  const result = await runGate(api, {
    eventName: 'issues',
    event: { issue: { number: 1 } },
    discover: true,
  });

  assert.equal(result.status, 1);
  assert.equal(result.stdout.trim(), '');
  assert.match(result.stderr, /GitHub API 500/);
});

test('scans open PRs beyond the newest twenty during fallback discovery', async (t) => {
  const prs = Array.from({ length: 21 }, (_, index) => ({
    number: index + 1,
    body: index === 20 ? 'Refs #1' : 'Refs #999',
    head: { sha: `head-${index + 1}` },
  }));
  let closingReads = 0;
  const api = await startFakeApi((request) => {
    if (request.path === `/repos/${REPOSITORY}/issues/1/timeline`) {
      return reply(500, { message: 'temporary failure' });
    }
    if (request.path === `/repos/${REPOSITORY}/pulls`) {
      return reply(200, prs);
    }
    if (request.method === 'POST' && request.path === '/graphql') {
      closingReads += 1;
      return closingRefs([]);
    }
    return undefined;
  });
  t.after(() => api.close());

  const result = await runGate(api, {
    eventName: 'issues',
    event: { issue: { number: 1 } },
    discover: true,
  });

  assert.equal(result.status, 0, result.stderr);
  const discovered = JSON.parse(result.stdout.trim());
  assert.deepEqual(discovered.prs, [{ number: 21, head: 'head-21' }]);
  assert.equal(discovered.complete, true);
  assert.equal(closingReads, 20);
});

test('reports incomplete discovery while still returning known linked PRs', async (t) => {
  const prs = Array.from({ length: 21 }, (_, index) => ({
    number: index + 1,
    body: index === 20 ? 'Refs #1' : 'Refs #999',
    head: { sha: `head-${index + 1}` },
  }));
  const api = await startFakeApi((request) => {
    if (request.path === `/repos/${REPOSITORY}/issues/1/timeline`) {
      return reply(500, { message: 'temporary failure' });
    }
    if (request.path === `/repos/${REPOSITORY}/pulls`) {
      return reply(200, prs);
    }
    if (request.method === 'POST' && request.path === '/graphql') {
      const { number } = JSON.parse(request.body).variables;
      if (number === 1) return reply(500, { message: 'temporary failure' });
      return closingRefs([]);
    }
    return undefined;
  });
  t.after(() => api.close());

  const result = await runGate(api, {
    eventName: 'issues',
    event: { issue: { number: 1 } },
    discover: true,
  });

  assert.equal(result.status, 0, result.stderr);
  const discovered = JSON.parse(result.stdout.trim());
  assert.deepEqual(discovered.prs, [{ number: 21, head: 'head-21' }]);
  assert.equal(discovered.complete, false);
  assert.match(result.stderr, /Incomplete linked-PR discovery/);
});

test('fails the check when the PR changes during evaluation', async (t) => {
  const checks = [];
  const patches = [];
  let prReads = 0;
  const api = await startFakeApi((request) => {
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/pulls/77`) {
      prReads += 1;
      return reply(200, pullRequest(prReads === 1 ? 'Fixes #1' : 'Fixes #1\n\nedited'));
    }
    if (request.method === 'POST' && request.path === '/graphql') {
      return closingRefs([]);
    }
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/issues/1`) {
      return reply(200, issue());
    }
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/commits/current-head/check-runs`) {
      return reply(200, { check_runs: checks });
    }
    if (request.method === 'POST' && request.path === `/repos/${REPOSITORY}/check-runs`) {
      const created = { id: 1, name: 'issue-gate', external_id: 'issue-gate:77' };
      checks.push(created);
      return reply(201, created);
    }
    if (request.method === 'PATCH' && request.path === `/repos/${REPOSITORY}/check-runs/1`) {
      patches.push(JSON.parse(request.body));
      return reply(200, {});
    }
    return undefined;
  });
  t.after(() => api.close());

  const result = await runGate(api, {
    eventName: 'pull_request_target',
    event: { pull_request: { number: 77 } },
    target: 77,
  });

  assert.equal(result.status, 1);
  const completed = patches.filter((patch) => patch.status === 'completed');
  assert.equal(completed.length, 1);
  assert.equal(completed[0].conclusion, 'failure');
  assert.equal(completed[0].output.title, 'PR changed during evaluation');
});

test('completes the check as a gate error when the final state read fails', async (t) => {
  const checks = [];
  const patches = [];
  let prReads = 0;
  const api = await startFakeApi((request) => {
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/pulls/77`) {
      prReads += 1;
      if (prReads === 1) return reply(200, pullRequest());
      return reply(500, { message: 'temporary failure' });
    }
    if (request.method === 'POST' && request.path === '/graphql') {
      return closingRefs([]);
    }
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/issues/1`) {
      return reply(200, issue());
    }
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/commits/current-head/check-runs`) {
      return reply(200, { check_runs: checks });
    }
    if (request.method === 'POST' && request.path === `/repos/${REPOSITORY}/check-runs`) {
      const created = { id: 1, name: 'issue-gate', external_id: 'issue-gate:77' };
      checks.push(created);
      return reply(201, created);
    }
    if (request.method === 'PATCH' && request.path === `/repos/${REPOSITORY}/check-runs/1`) {
      patches.push(JSON.parse(request.body));
      return reply(200, {});
    }
    return undefined;
  });
  t.after(() => api.close());

  const result = await runGate(api, {
    eventName: 'pull_request_target',
    event: { pull_request: { number: 77 } },
    target: 77,
  });

  assert.equal(result.status, 1);
  const completed = patches.filter((patch) => patch.status === 'completed');
  assert.equal(completed.length, 1);
  assert.equal(completed[0].conclusion, 'failure');
  assert.equal(completed[0].output.title, 'Gate error');
});

test('completes the check as a gate error when the initial PR read fails', async (t) => {
  const checks = [];
  const patches = [];
  const api = await startFakeApi((request) => {
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/pulls/77`) {
      return reply(500, { message: 'temporary failure' });
    }
    if (request.method === 'GET' && request.path === `/repos/${REPOSITORY}/commits/current-head/check-runs`) {
      return reply(200, { check_runs: checks });
    }
    if (request.method === 'POST' && request.path === `/repos/${REPOSITORY}/check-runs`) {
      const created = { id: 1, name: 'issue-gate', external_id: 'issue-gate:77' };
      checks.push(created);
      return reply(201, created);
    }
    if (request.method === 'PATCH' && request.path === `/repos/${REPOSITORY}/check-runs/1`) {
      patches.push(JSON.parse(request.body));
      return reply(200, {});
    }
    return undefined;
  });
  t.after(() => api.close());

  const result = await runGate(api, {
    eventName: 'pull_request_target',
    event: { pull_request: { number: 77 } },
    target: 77,
  });

  assert.equal(result.status, 1);
  assert.ok(api.requests.some((request) => request.path === `/repos/${REPOSITORY}/commits/current-head/check-runs`));
  const completed = patches.filter((patch) => patch.status === 'completed');
  assert.equal(completed.length, 1);
  assert.equal(completed[0].conclusion, 'failure');
  assert.equal(completed[0].output.title, 'Gate error');
});

test('the discovery step propagates a gate script failure', () => {
  const workflow = fs.readFileSync(path.join(ROOT, '.github', 'workflows', 'issue-gate.yml'), 'utf8');
  assert.match(workflow, /set -euo pipefail/);
  assert.match(workflow, /result=\$\(node scripts\/issue-gate\.mjs --discover\)/);
  assert.doesNotMatch(workflow, /echo "prs=\$\(node/);
  assert.ok(workflow.includes('TARGET_HEAD: ${{ matrix.pr.head }}'));
  assert.ok(workflow.includes('TARGET_HEAD: ${{ github.event.pull_request.head.sha }}'));
  assert.match(workflow, /name: coverage/);
  assert.match(workflow, /needs\.discover\.outputs\.complete != 'true'/);
});