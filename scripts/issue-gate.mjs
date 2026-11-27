import fs from 'node:fs';
import { fileURLToPath } from 'node:url';

const DEFAULT_API = 'https://api.github.com';
const CHECK_NAME = 'issue-gate';
const MAX_REFS = 20;
const MAX_PAGES = 10;
const MAX_LINKED_PRS = 20;
const MAX_SUMMARY = 60_000;
const SCAN_BATCH = 10;
const RETRY_STATUS = new Set([429, 500, 502, 503, 504]);

function apiUrl() {
  return (process.env.GITHUB_API_URL || DEFAULT_API).replace(/\/$/, '');
}

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
  return source?.issue?.repository_url === `${apiUrl()}/repos/${repository}`;
}

function required(name) {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set`);
  return value;
}

function retryAttempts() {
  const attempts = Number(process.env.ISSUE_GATE_RETRY_ATTEMPTS ?? 3);
  return Number.isInteger(attempts) && attempts > 0 ? attempts : 3;
}

function retryDelayMs() {
  const delay = Number(process.env.ISSUE_GATE_RETRY_DELAY_MS ?? 500);
  return Number.isFinite(delay) && delay >= 0 ? delay : 500;
}

function isRetryable(error) {
  return RETRY_STATUS.has(error.status) || error.name === 'TypeError';
}

async function withRetry(operation) {
  const attempts = retryAttempts();
  for (let attempt = 1; ; attempt += 1) {
    try {
      return await operation();
    } catch (error) {
      if (!isRetryable(error) || attempt >= attempts) throw error;
      await new Promise((resolve) => setTimeout(resolve, retryDelayMs() * attempt));
    }
  }
}

async function api(path, options = {}) {
  return withRetry(async () => {
    const response = await fetch(`${apiUrl()}${path}`, {
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
  });
}

async function pages(path) {
  const items = [];
  for (let page = 1; page <= MAX_PAGES; page += 1) {
    const separator = path.includes('?') ? '&' : '?';
    const batch = await api(`${path}${separator}per_page=100&page=${page}`);
    items.push(...batch);
    if (batch.length < 100) return items;
  }
  throw new Error(`Too many pages: ${path}`);
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

async function scanOpenPrsForIssue(issueNumber, prs) {
  const linked = new Set();
  const problems = [];
  for (let index = 0; index < prs.length; index += SCAN_BATCH) {
    const batch = prs.slice(index, index + SCAN_BATCH);
    const matches = await Promise.all(
      batch.map(async (pr) => {
        const refs = new Set(parseRefs(pr.body ?? ''));
        if (!refs.has(issueNumber)) {
          try {
            for (const number of await getClosingRefs(pr.number)) refs.add(number);
          } catch (error) {
            if (error.status !== 404) {
              problems.push(`#${pr.number}: ${error.message}`);
              console.error(`Could not read closing references for #${pr.number}: ${error.message}`);
            }
            return null;
          }
        }
        return refs.has(issueNumber) ? pr.number : null;
      }),
    );
    for (const number of matches) if (number) linked.add(number);
  }
  const numbers = [...linked].sort((left, right) => left - right);
  if (numbers.length > MAX_LINKED_PRS) {
    problems.push(`More than ${MAX_LINKED_PRS} linked open pull requests`);
  }
  return {
    numbers: numbers.slice(0, MAX_LINKED_PRS),
    complete: problems.length === 0,
    problems,
  };
}

async function getLinkedOpenPrs(issueNumber) {
  const repository = required('GITHUB_REPOSITORY');
  const openPrs = await pages(`/repos/${repository}/pulls?state=open`);
  const byNumber = new Map(openPrs.map((pr) => [pr.number, pr]));
  let numbers;
  let complete = true;
  const problems = [];
  try {
    const events = await pages(`/repos/${repository}/issues/${issueNumber}/timeline`);
    const linked = new Set();
    for (const event of events) {
      if (event.event !== 'cross-referenced') continue;
      const source = event.source;
      if (!sameRepository(source, repository)) continue;
      if (source.issue.pull_request && source.issue.state === 'open') linked.add(source.issue.number);
    }
    numbers = [...linked].sort((left, right) => left - right);
    if (numbers.length > MAX_LINKED_PRS) {
      problems.push(`More than ${MAX_LINKED_PRS} linked open pull requests`);
      complete = false;
      numbers = numbers.slice(0, MAX_LINKED_PRS);
    }
  } catch (error) {
    console.error(`Timeline read failed (${error.message}); scanning open PRs.`);
    const scan = await scanOpenPrsForIssue(issueNumber, openPrs);
    numbers = scan.numbers;
    complete = scan.complete;
    problems.push(...scan.problems);
  }
  const prs = [];
  for (const number of numbers) {
    const pr = byNumber.get(number);
    if (!pr) continue;
    const head = pr.head?.sha;
    if (!head) {
      problems.push(`#${number} has no head SHA`);
      continue;
    }
    prs.push({ number, head });
  }
  return { prs, complete: complete && problems.length === 0, problems };
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
  const numbers = [...new Set([...refs, ...closing.slice(0, MAX_REFS)])].slice(0, MAX_REFS * 2);
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

async function findCheckRuns(pr) {
  const repository = required('GITHUB_REPOSITORY');
  const response = await api(
    `/repos/${repository}/commits/${pr.head}/check-runs?check_name=${CHECK_NAME}&filter=all&per_page=100`,
  );
  const runs = Array.isArray(response) ? response : response.check_runs ?? [];
  return runs
    .filter((run) => run.name === CHECK_NAME && (!run.external_id || run.external_id === checkId(pr)))
    .sort((left, right) => right.id - left.id);
}

async function patchCheckRun(id, payload) {
  const repository = required('GITHUB_REPOSITORY');
  await api(`/repos/${repository}/check-runs/${id}`, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload),
  });
}

async function markCheckRunsInProgress(pr, runs, output, details) {
  try {
    await Promise.all(
      runs.map((run) =>
        patchCheckRun(run.id, {
          status: 'in_progress',
          external_id: checkId(pr),
          output,
          ...(details ? { details_url: details } : {}),
        }),
      ),
    );
    return runs.map((run) => run.id);
  } catch (error) {
    if (error.status === 422) return [];
    throw error;
  }
}

async function beginCheckRun(pr) {
  const repository = required('GITHUB_REPOSITORY');
  const details = detailsUrl();
  const output = {
    title: 'Evaluating linked issue',
    summary: 'Checking the PR body and linked issue labels.',
  };
  let existing = [];
  try {
    existing = await findCheckRuns(pr);
  } catch (error) {
    console.error(`Could not read the existing issue-gate check: ${error.message}`);
  }
  if (existing.length) {
    const ids = await markCheckRunsInProgress(pr, existing, output, details);
    if (ids.length) return ids;
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
  const ids = new Set([created.id]);
  try {
    const reconciled = await findCheckRuns(pr);
    if (reconciled.length) {
      for (const id of await markCheckRunsInProgress(pr, reconciled, output, details)) ids.add(id);
    }
  } catch (error) {
    console.error(`Could not reconcile issue-gate checks: ${error.message}`);
  }
  return [...ids];
}

async function finishCheckRun(pr, ids, result) {
  const details = detailsUrl();
  const targets = new Set(ids);
  try {
    for (const run of await findCheckRuns(pr)) targets.add(run.id);
  } catch (error) {
    console.error(`Could not reconcile issue-gate checks: ${error.message}`);
  }
  const results = await Promise.allSettled(
    [...targets].map((id) =>
      patchCheckRun(id, {
        status: 'completed',
        conclusion: result.ok ? 'success' : 'failure',
        external_id: checkId(pr),
        completed_at: new Date().toISOString(),
        output: checkOutput(result),
        ...(details ? { details_url: details } : {}),
      }),
    ),
  );
  const rejected = results.find((outcome) => outcome.status === 'rejected');
  if (rejected) throw rejected.reason;
}

async function failCheckRun(pr, ids, error) {
  try {
    await finishCheckRun(pr, ids, { ok: false, title: 'Gate error', summary: error.message });
  } catch (finishError) {
    console.error(`Could not record the gate failure: ${finishError.message}`);
  }
}

async function evaluateStable(pr) {
  const result = await evaluate(pr);
  const current = await getPr(pr.number);
  if (current.head !== pr.head || current.body !== pr.body || current.state !== pr.state) {
    return {
      ok: false,
      title: 'PR changed during evaluation',
      summary: 'The pull request changed while the gate evaluated it; the next event re-evaluates the current revision.',
    };
  }
  return result;
}

async function main() {
  const eventPath = required('GITHUB_EVENT_PATH');
  const event = JSON.parse(fs.readFileSync(eventPath, 'utf8'));
  const eventName = required('GITHUB_EVENT_NAME');

  if (process.argv.includes('--discover')) {
    if (eventName !== 'issues') throw new Error(`--discover requires an issues event, got ${eventName}`);
    const result = await getLinkedOpenPrs(event.issue.number);
    if (!result.complete) {
      console.error(`Incomplete linked-PR discovery: ${result.problems.join('; ')}`);
    }
    console.log(JSON.stringify(result));
    return;
  }

  const target = Number(required('TARGET_PR'));
  if (!Number.isInteger(target) || target <= 0) throw new Error(`Invalid TARGET_PR: ${target}`);
  const eventHead =
    eventName === 'pull_request_target' && event.pull_request?.number === target
      ? event.pull_request.head?.sha
      : undefined;
  const knownHead = process.env.TARGET_HEAD || eventHead;
  if (!knownHead) throw new Error(`No head SHA for PR #${target}`);
  const knownPr = { number: target, head: knownHead };

  let checks;
  try {
    checks = await beginCheckRun(knownPr);
    const pr = await getPr(target);
    if (pr.head !== knownHead) {
      const changed = {
        ok: false,
        title: 'PR head changed',
        summary: 'The pull request head changed before evaluation; the current revision is evaluated by its own event.',
      };
      const currentChecks = await beginCheckRun(pr);
      await finishCheckRun(pr, currentChecks, changed);
      await finishCheckRun(knownPr, checks, changed);
      console.log(`#${target}: FAIL: ${changed.title}\n${changed.summary}`);
      process.exitCode = 1;
      return;
    }
    const result = await evaluateStable(pr);
    await finishCheckRun(pr, checks, result);
    console.log(`#${target}: ${result.ok ? 'PASS' : 'FAIL'}: ${result.title}\n${result.summary}`);
    process.exitCode = result.ok ? 0 : 1;
  } catch (error) {
    if (checks) await failCheckRun(knownPr, checks, error);
    throw error;
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
