# github-watch

A Session row that watches GitHub issues and pull requests and delivers changes
back into the Session as input messages.

It cannot use events for that: under rutis 0.13 scopes a process-level listener
sits above the Session and delivery only travels upward, and `timeline.append`
writes domain events, not adopted input. A Session row calling `sessions.input`
is the supported path in both 0.12 and 0.15.

## Modes

`InputMessageData` decides what an input does:

| Mode | Meaning |
| --- | --- |
| `steer` | Applies at the next tool-batch boundary of the running turn; wakes an idle branch. |
| `queue` | Waits for the current turn to end, then runs as the next turn. |
| `append` | Never drives a turn; joins the next request built (context only). |

The default policy: a **review** steers, a **comment** queues (configure `urgent`
to steer), a **state** change queues, and other metadata is only appended. One
poll delivers at most one input per watched item, with the highest mode of the
changes it found.

## Tool

The model controls subscriptions with the `github_watch` tool:

```json
{"action": "watch", "repo": "arcships/dim", "number": 361}
{"action": "watch", "repo": "arcships/dim", "number": 361, "report_initial": true}
{"action": "unwatch", "repo": "arcships/dim", "number": 361}
{"action": "list"}
{"action": "poll"}
```

`watch` records the current state as the baseline; later polls deliver only
changes. `report_initial: true` delivers the current state once as `append`
(context only, it never starts a turn), because the tool call itself is what
asked for it. `poll` runs one round now (useful after starting a job).

## Delivery

Each change becomes one `sessions.input` with:

- `submissionId` = `github-watch:<owner/repo#n>:<hash(signature)>`, so a retried
  or reloaded poll does not deliver twice (`sessions.lookup` guards it);
- a `<dim-github-watch repo="…" number="…">` raw block, so history and clients
  read it as a source input rather than something the operator typed;
- text naming the change (`review changes_requested by …`, `comment by …`).

## Credentials and limits

- Reads use the `gh` CLI, so it uses the user's existing `gh auth`; the plugin
  runs `gh api` from the runtime process (config-driven command, no per-call
  approval, ADR 003).
- Polling: `intervalSeconds` (default 60, minimum 1). There is no webhook path.
- Subscriptions live in the row's memory: a reload or Session restart drops
  them. Persisting them through `pluginFiles` is a follow-up.
- Only issues/PRs are watched, not CI checks (the checks API needs GraphQL);
  `gh pr checks` or `gh run watch` cover that.

## Wiring

Add `github-watch.json` to the Session TypeScript rows of the profile that
should offer it. The row injects `sessions` and provides
`toolContribution:githubWatch`.
