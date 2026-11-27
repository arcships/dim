import fs from 'node:fs';
import { fileURLToPath } from 'node:url';

const API = 'https://api.github.com';

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
      for (const match of line.matchAll(/#(\d+)/g)) refs.add(Number(match[1]));
    }
  }
  return [...refs];
}

export function isAccepted(labels) {
  return labels.some((label) => label === 'P0' || label === 'P1' || label === 'P2');
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
  const events = await pages(`/repos/${required('GITHUB_REPOSITORY')}/issues/${issueNumber}/timeline`);
  const numbers = new Set();
  for (const event of events) {
    if (event.event !== 'cross-referenced') continue;
    const source = event.source?.issue;
    if (source?.pull_request && source.state === 'open') numbers.add(source.number);
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
  const numbers = [...new Set([...closing, ...refs])];
  if (!numbers.length) {
    return {
      ok: false,
      title: 'No linked issue',
      summary: 'Add `Fixes #N`, `Closes #N`, `Resolves #N` or `Refs #N` with an accepted issue (P0, P1 or P2).',
    };
  }

  const accepted = [];
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
        accepted.push(number);
      } else {
        rejected.push(`#${number} has [${issue.labels.join(', ') || 'no labels'}]`);
      }
    } catch (error) {
      unreadable.push(`#${number}: ${error.message}`);
    }
  }

  if (accepted.length) {
    return {
      ok: true,
      title: 'Linked accepted issue',
      summary: `Accepted: ${accepted.map((number) => `#${number}`).join(', ')}.`,
    };
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

async function createCheckRun(head, result) {
  await api(`/repos/${required('GITHUB_REPOSITORY')}/check-runs`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      name: 'issue-gate',
      head_sha: head,
      status: 'completed',
      conclusion: result.ok ? 'success' : 'failure',
      output: { title: result.title, summary: result.summary },
    }),
  });
}

async function main() {
  const eventPath = required('GITHUB_EVENT_PATH');
  const event = JSON.parse(fs.readFileSync(eventPath, 'utf8'));
  const eventName = required('GITHUB_EVENT_NAME');

  if (eventName === 'pull_request_target') {
    const pr = await getPr(event.pull_request.number);
    const result = await evaluate(pr);
    console.log(`${result.ok ? 'PASS' : 'FAIL'}: ${result.title}\n${result.summary}`);
    process.exitCode = result.ok ? 0 : 1;
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
      try {
        const pr = await getPr(number);
        const result = await evaluate(pr);
        await createCheckRun(pr.head, result);
        console.log(`#${number}: ${result.ok ? 'PASS' : 'FAIL'} — ${result.title}`);
      } catch (error) {
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
