# Agents `sessions` service

Status: replaced by [ADR 027](../../adr/027-cross-scope-access-by-id.md) and the [Agents contract](../plugins/Agents.md). This page keeps the lifecycle, submission-identity and authority facts that outlive the `sessions` methods, and lists where each old decision now lives.

## 1. Lifecycle

- **Ownership.** Agents owns a launched Session's instance and record like any other Session: clients list it, a user can attach to it, answer its approvals, stop, close or delete it. The caller owns only its own records that name the Session and the inputs it wrote ([Agents](../plugins/Agents.md)). Closing or deleting the starter does not touch a launched Session.
- **Loading.** A Session stays loaded while a lease holds it or its work runs; Agents unloads it once no lease has held it for `idle_unload_after` and it is idle ([idle unload](idle-unload.md), [lifecycle §5](../../architecture/rust-agent/lifecycle.md#5-stop-与-close)). The next call restores it. A lease is held only while a call runs ([Agents: `acquire`](../plugins/Agents.md#operations)).
- **Reload.** A reload of Agents' directory logic fails the calls in flight and the calls that arrive while it settles, and never reloads a caller; an admitted creation or delete replies its own outcome ([idle unload](idle-unload.md), [#35](https://github.com/arcships/dim/issues/35)). Callers resolve an uncertain write by resubmitting the same `submissionId` (§2).
- **Delete.** A delete takes the Session's admission gate and closes it; calls in flight on that Session end `session_closed`, except an InputQueue command written before the close, which InputQueue decides. A launched Session whose starter is deleted keeps running ([session deletion and fork](session-deletion-and-fork.md)).
- **Deleting run Sessions.** The consumer that launched a Session deletes it when it drops the last record that names it, after the run is terminal and the Session is stopped; the record drops after the delete succeeds, so a failed delete retries. Scheduler and Workflow keep the retention that bounds this ([Scheduler §A8](../plugins/Scheduler.md), [Workflow §A9](../plugins/Workflow.md)). A user keeps a run by forking it.

## 2. Submission identity

`<source>:<uuid>`, generated per logical submission. A branch accepts each identity once: a resubmission writes nothing and replies the first input's receipt, also after the input is consumed or withdrawn, and also while the first write is still unpublished. The identity is the `submission_id` field of the committed `input/message`, so it commits, forks and is deleted with the input; no separate record exists ([#32](https://github.com/arcships/dim/issues/32)). A caller that lost a reply resubmits the same identity to [InputQueue](../plugins/InputQueue.md)'s submit and receives the existing receipt; several inputs sharing an identity are InputQueue's `ambiguous`.

## 3. Security and authority

- **Trust.** These operations serve trusted in-process plugins of the control node's process. Ids are not authorization, and a trusted in-process plugin is not sandboxed by its dependency declarations (AGENTS.md rule 11, ADR 027).
- **Policy bound.** A creation's bound keeps a Session within its starter's current policy, or within the host defaults when the request names no starter. It is a convention for well-behaved callers, not a security boundary: the caller supplies `starter` itself, and without one the bound is the host defaults. A user can still change a Session's policy through `session.permission.update`.
- **Approval.** Tool calls in a launched Session go through Approval and Permission under its own policy. A run started by configuration needs no host-local approval ([ADR 003 D2](../../adr/003-approval-scope.md#d2-no-host-local-approval-for-configuration)).
- **Content.** Agents never parses markers or `submissionId`s. Inputs are replayed into model context, so callers must not put credentials in them ([contract §2](../contracts/session-access.md#2-security)).

## 4. Where each old decision lives

| Old decision or method | Now |
| --- | --- |
| D1 directory part serves the face; D2 one implementation on the index | [Agents](../plugins/Agents.md): one owner, one face, no second binding. |
| D3 policy cap; D4 host defaults as the bound; D5 starter read failure; D6 `launch` reply | [Agents: `create_session`](../plugins/Agents.md#operations). |
| D7 `model` as `{providerId, modelId}` | [Agents](../plugins/Agents.md): a resolved selection; Provider's `resolve`. |
| D8 `input` replies after publication; D9 `lookup`; D11 `history`; D10 `idle`; `pending` | [InputQueue](../plugins/InputQueue.md) for input, receipt and pending; [TimelineTree](../plugins/TimelineTree.md) for the history page and the default line; [Loop](../plugins/Loop.md) for the turn and its stop. |
| D12 no `close`; D13 no `fork`; D14 consumer deletes | [Agents: `close_session`, `fork_session`, `delete_session`](../plugins/Agents.md#operations). |
| §3 spike findings 1–4, 7–8 (method behaviour) | Replaced with the methods; findings 5 (a language reaches a Rust service by name) and 6 (a loaded Session's cost) stay valid and are recorded above. |
| §12 changes to other documents | Delivered; the documents now link the owners above. |

## 5. Requirements raised earlier

- InputQueue withdrawal (#58, S1) and the optional `reason` on `input/withdraw`: delivered, owned by [InputQueue](../plugins/InputQueue.md).
- TypeScript bridge (#63, C4): delivered; the mechanism's id path is [ADR 027](../../adr/027-cross-scope-access-by-id.md).
- Idle unload for launched Sessions (#126, S3): delivered; the rules are in [idle unload](idle-unload.md).
