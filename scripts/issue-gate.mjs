import fs from 'node:fs';
import { fileURLToPath } from 'node:url';

const API = 'https://api.github.com';
const CHECK_NAME = 'issue-gate';
const MAX_REFS = 20;
const MAX_SUMMARY = 60_000;

export function stripNonLinks(body) {
  let text = String(body ?? '');
  text = text.replace(/```[\s\S]*?```/g, '');
  text = text.replace(/~~~[\s\S]*?~~~/g, '');
  text = text.replace(/<!--[\s\S]*?-->/g, '');
  text = text.replace(/`[^`\n]*`/g, '');
  return text
    .split(/\r?\n/)
    .filter((line) => !/^\s*>/.test(line))
    .join('\n');
}

export function parseRefs(body) {
  const refs = new Set();
  for (const rawLine of stripNonLinks(body).split(/\r?\n/)) {
    const line = rawLine.replace(/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+#\d+/g, '');
    if (/(^|[^A-Za-z0-9_])(fixes|closes|resolves|refs):?\s+#\d+/i.test(line)) {
      for (const match of line.matchAll(/#(\d+)/g)) {
        if (refs.size >= MAX_REFS) break;
        refs.add(Number(match[1]));
      }
    }
  }
  return [...refs];
}

export function isAccepted(labels) {
  return labels.some((label) => label === 'P0' || label === 'P1' || label === 'P2');
}

export function sameRepository(source, repository) {
  return source?.issue?.repository_url === `${API}/repos/${repository}`;
}

function required(name) {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set`);
  return value;
}

async function api(path, options = {}) {
  const response = await fetch(`${API}${path}`, {
    ...options,
    headers: {
      Authorization: `Bearer ${required('GITHUB_TOKEN')}`,
      Accept: 'application/vnd.github+json',
      'User-Agent': 'dim-issue-gate',
      ...(options.headers ?? {}),
    },
  });
  if (!response.ok) {
    const body = await response.text();
    const error = new Error(`GitHub API ${response.status} ${path}: ${body.slice(0, 300)}`);
    error.status = response.status;
    throw error;
  }
  return response.status === 204 ? null : response.json();
}

async function pages(path) {
  const items = [];
  for (let page = 1; ; page += 1) {
    const separator = path.includes('?') ? '&' : '?';
    const batch = await api(`${path}${separator}per_page=100&page=${page}`);
    items.push(...batch);
    if (batch.length < 100) return items;
  }
}

async function getPr(number) {
  const pr = await api(`/repos/${required('GITHUB_REPOSITORY')}/pulls/${number}`);
  return { number, body: pr.body ?? '', head: pr.head.sha, state: pr.state };
}

async function getIssue(number) {
  const repo = required('GITHUB_REPOSITORY');
  try {
    const issue = await api(`/repos/${repo}/issues/${number}`);
    if (issue.pull_request) return { kind: 'pr' };
    return { kind: 'issue', labels: issue.labels.map((label) => label.name) };
  } catch (error) {
    if (error.status === 404) return { kind: 'missing' };
    throw error;
  }
}

async function getClosingRefs(number) {
  const [owner, repo] = required('GITHUB_REPOSITORY').split('/');
  const query = `query($owner: String!, $repo: String!, $number: Int!) {
    repository(owner: $owner, name: $repo) {
      pullRequest(number: $number) {
        closingIssuesReferences(first: 50) {
          nodes { number repository { nameWithOwner } }
        }
      }
    }
  }`;
  const result = await api('/graphql', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ query, variables: { owner, repo, number } }),
  });
  if (result.errors?.length) throw new Error(result.errors.map((error) => error.message).join('; '));
  return result.data.repository.pullRequest.closingIssuesReferences.nodes
    .filter((node) => node.repository?.nameWithOwner === required('GITHUB_REPOSITORY'))
    .map((node) => node.number);
}

async function getLinkedOpenPrs(issueNumber) {
  const repository = required('GITHUB_REPOSITORY');
  const events = await pages(`/repos/${repository}/issues/${issueNumber}/timeline`);
  const numbers = new Set();
  for (const event of events) {
    if (event.event !== 'cross-referenced') continue;
    const source = event.source;
    if (!sameRepository(source, repository)) continue;
    if (source.issue.pull_request && source.issue.state === 'open') numbers.add(source.issue.number);
  }
  return [...numbers];
}

async function evaluate(pr) {
  let closing;
  try {
    closing = await getClosingRefs(pr.number);
  } catch (error) {
    return {
      ok: false,
      title: 'Could not read closing references',
      summary: error.message,
    };
  }
  const refs = parseRefs(pr.body);
  const numbers = [...new Set([...closing, ...refs])].slice(0, MAX_REFS);
  if (!numbers.length) {
    return {
      ok: false,
      title: 'No linked issue',
      summary: 'Add `Fixes #N`, `Closes #N`, `Resolves #N` or `Refs #N` with an accepted issue (P0, P1 or P2).',
    };
  }

  const rejected = [];
  const unreadable = [];
  for (const number of numbers) {
    try {
      const issue = await getIssue(number);
      if (issue.kind === 'pr') {
        rejected.push(`#${number} is a pull request, not an issue`);
      } else if (issue.kind === 'missing') {
        unreadable.push(`#${number} does not exist in this repository`);
      } else if (isAccepted(issue.labels)) {
        return {
          ok: true,
          title: 'Linked accepted issue',
          summary: `Accepted: #${number}.`,
        };
      } else {
        rejected.push(`#${number} has [${issue.labels.join(', ') || 'no labels'}]`);
      }
    } catch (error) {
      unreadable.push(`#${number}: ${error.message}`);
    }
  }

  if (unreadable.length && !rejected.length) {
    return {
      ok: false,
      title: 'Could not read linked issues',
      summary: unreadable.join('\n'),
    };
  }
  return {
    ok: false,
    title: 'No accepted issue',
    summary: [...rejected, ...unreadable].join('\n'),
  };
}

function checkOutput(result) {
  return {
    title: String(result.title).slice(0, 200),
    summary: String(result.summary ?? '').slice(0, MAX_SUMMARY),
  };
}

function detailsUrl() {
  const server = process.env.GITHUB_SERVER_URL;
  const repository = process.env.GITHUB_REPOSITORY;
  const run = process.env.GITHUB_RUN_ID;
  return server && repository && run ? `${server}/${repository}/actions/runs/${run}` : undefined;
}

function checkId(pr) {
  return `${CHECK_NAME}:${pr.number}`;
}

async function findCheckRun(pr) {
  const repository = required('GITHUB_REPOSITORY');
  const response = await api(
    `/repos/${repository}/commits/${pr.head}/check-runs?check_name=${CHECK_NAME}&per_page=100`,
  );
  const runs = Array.isArray(response) ? response : response.check_runs ?? [];
  return (
    runs.find((run) => run.name === CHECK_NAME && run.external_id === checkId(pr)) ??
    runs.find((run) => run.name === CHECK_NAME && !run.external_id) ??
    null
  );
}

async function beginCheckRun(pr) {
  const repository = required('GITHUB_REPOSITORY');
  const details = detailsUrl();
  const output = {
    title: 'Evaluating linked issue',
    summary: 'Checking the PR body and linked issue labels.',
  };
  const existing = await findCheckRun(pr).catch((error) => {
    console.error(`Could not read the existing issue-gate check: ${error.message}`);
    return null;
  });
  if (existing) {
    try {
      await api(`/repos/${repository}/check-runs/${existing.id}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ status: 'in_progress', external_id: checkId(pr), output, ...(details ? { details_url: details } : {}) }),
      });
      return existing.id;
    } catch (error) {
      if (error.status !== 422) throw error;
    }
  }
  const created = await api(`/repos/${repository}/check-runs`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      name: CHECK_NAME,
      head_sha: pr.head,
      external_id: checkId(pr),
      status: 'in_progress',
      output,
      ...(details ? { details_url: details } : {}),
    }),
  });
  return created.id;
}

async function finishCheckRun(id, result) {
  const repository = required('GITHUB_REPOSITORY');
  const details = detailsUrl();
  await api(`/repos/${repository}/check-runs/${id}`, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      status: 'completed',
      conclusion: result.ok ? 'success' : 'failure',
      completed_at: new Date().toISOString(),
      output: checkOutput(result),
      ...(details ? { details_url: details } : {}),
    }),
  });
}

async function failCheckRun(id, error) {
  try {
    await finishCheckRun(id, { ok: false, title: 'Gate error', summary: error.message });
  } catch (finishError) {
    console.error(`Could not record the gate failure: ${finishError.message}`);
  }
}

async function main() {
  const eventPath = required('GITHUB_EVENT_PATH');
  const event = JSON.parse(fs.readFileSync(eventPath, 'utf8'));
  const eventName = required('GITHUB_EVENT_NAME');

  if (eventName === 'pull_request_target') {
    const eventPr = event.pull_request;
    const pr = {
      number: eventPr.number,
      body: eventPr.body ?? '',
      head: eventPr.head.sha,
      state: eventPr.state,
    };
    let check;
    try {
      check = await beginCheckRun(pr);
      const result = await evaluate(pr);
      await finishCheckRun(check, result);
      console.log(`${result.ok ? 'PASS' : 'FAIL'}: ${result.title}\n${result.summary}`);
      process.exitCode = result.ok ? 0 : 1;
    } catch (error) {
      if (check) await failCheckRun(check, error);
      throw error;
    }
    return;
  }

  if (eventName === 'issues') {
    const issueNumber = event.issue.number;
    const prs = await getLinkedOpenPrs(issueNumber);
    if (!prs.length) {
      console.log(`No open linked PR for #${issueNumber}.`);
      return;
    }
    let failed = false;
    for (const number of prs) {
      let check;
      try {
        const pr = await getPr(number);
        check = await beginCheckRun(pr);
        const result = await evaluate(pr);
        await finishCheckRun(check, result);
        console.log(`#${number}: ${result.ok ? 'PASS' : 'FAIL'} — ${result.title}`);
        if (!result.ok) failed = true;
      } catch (error) {
        if (check) await failCheckRun(check, error);
        console.error(`#${number}: could not update issue-gate: ${error.message}`);
        failed = true;
      }
    }
    if (failed) process.exitCode = 1;
    return;
  }

  throw new Error(`Unsupported event: ${eventName}`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch((error) => {
    console.error(error.message);
    process.exit(1);
  });
}
