# Report plugin

The [target Session tree binding](../contracts/runtime-owner-operations.md) moves calls from `treeCommands` to `timelineTree` with the same arguments and replies. The catalog entry and consumer injections change together. References to `treeCommands` below describe the pre-migration binding.

Status: Draft. Scope: Session reports and a process entry for errors without a readable Session.
Issue: [#1319](https://github.com/arcships/dim/issues/1319).

## 1. Ownership

Report captures a bounded snapshot when a client opens the dialog. It owns evidence selection, filtering, the model-send preview, one-shot generation, and the final report and ZIP. The client owns the error hint or action, confirmation, and local save or sharing. App Server assembles the parts and admits RPC. The first integration uses both TUI renderers. Desktop and Web use the same report shapes and archive.

```text
TUI /report        Desktop or Web "Report this error"
     \                          /
      authenticated App Server connection
            |                         |
     session.report.*             report.*
     Session evidence             Error-only evidence
            \                         /
          Report preview, generation and ZIP
```

The Session part owns the draft and reads only the admitted Session. If its history read fails, the draft proceeds with an explicit missing-evidence reason and any captured error supplied by the client. If Session admission fails, the process part can prepare an error-only draft from a client-observed error. The process request accepts neither a Session nor a branch ID.

The error-only entry uses `managementMethods` on the local App Server connection and requires explicit admission on the authenticated local Web entry (§7). Paired connections can use `session.report.*` while Session admission succeeds. The paired error-only entry is a separate admission decision in §7. Preparing a report requires a live connection. A TUI that already received the generated ZIP can still save it after the connection closes.

## 2. Client interaction

In both TUI modes, `/report` opens a report for the current Session, including while a turn is running. It needs no captured error. Users can report an unsatisfactory answer or a repeating turn without waiting for a failure or stopping the turn. The dialog keeps the Session selected when it opened, including when preparing again after expiry. Returning to the composer and running `/report` again selects the Session shown at that time.

Both modes show `Run /report to create a report.` below reportable errors. When a Session is shown, the hint points to the Session report and its recent activity excerpt (§3). On Home, the command can report a conversation-open failure from that view through the process entry. Leaving a Session clears its eligibility as an error source for Home. With neither a Session nor a conversation-open failure to report, the command shows `No session to report.` Desktop and Web error actions remain bound to the selected error. A failure of the report flow stays inside the dialog.

```text
> /report

+-- Report this session ---------------------------------------+
| Session model provider/model                                 |
| Local ZIP. Nothing is uploaded.                              |
| Diagnostics only. Uncheck Redact to include recent messages. |
| > Model: Generate locally                                    |
|   Redact [x]                                                 |
|   Options                                                    |
|   Generate report                                            |
|   Close                                                      |
+--------------------------------------------------------------+

+-- Report ready ----------------------------------------------+
| Saved ZIP <ZIP path>                                         |
| Email the ZIP to feedback@arcships.ai                        |
| <Report title>                                               |
| ZIP contents: report.md, diagnostics.json                    |
| > Save ZIP                                                   |
|   Draft a GitHub issue                                       |
|   Give feedback on Discord                                   |
|   Close                                                      |
+--------------------------------------------------------------+
```

The error-only overlay uses `Report this error`. The Session overlay shows when the Timeline could not be read or the bounded excerpt has no reportable activity. Either state permits generation. Local generation is available on its own. Clients can expose a model picker once the one-shot operation in §7 is available. Its initial model is the adopted model of the referenced branch, or of the Session's main branch when there is no branch reference. It also offers models from the client's admitted catalog and **Generate locally**. Redaction starts checked on every new report: Generate then Save ZIP produces diagnostics only. Before generation, the Session dialog explains that turning redaction off may include sensitive text from recent user and assistant messages. An error-only report has no conversation to attach. Generating and saving never send a report to a feedback destination. Options accepts a short instruction about what to omit or keep. There are no required fields. With a model selected, the dialog exposes the exact model material and limits before Generate can send it. Local mode displays zero model requests. After a stale plan or a failed generation, the client offers to refresh the plan. When the draft expires, the client offers to prepare a report for the original target and reapplies the chosen options.

The overlay follows the TUI's focus and list conventions. Up and Down move between controls, and Tab and Shift+Tab cycle focus. Enter activates the focused control. Esc closes a picker first, cancels generation while busy, or closes the report. Focus starts on Model. The TUI shortens the error and report title to fit the overlay and keeps the focused action visible. The result lists the archive contents and shows the saved path so users can inspect the files with their own tools. Place `/report` immediately before `/exit` in both curated menus with the description `report the current session`, following the existing exact-name, Enter and Tab behavior. The CLI command owner owns the menu (§7). [TUI design](Tui.md), [command menu contract](../cli-commands.md#command-menu).

## 3. Evidence and redaction

For a report about the Session, the admitted Session is the entire client-supplied reference. The Report part captures recent activity from its main branch at the published head. Error actions can supply a typed Timeline failure reference with an observed-error fallback, or an observed error alone. A typed reference contains the branch and committed sequence in the admitted Session. An observed error contains a registered client operation, a stable error code if present, the observation time and the client version. The client keeps the displayed message for the dialog. The report projects a stable operation summary and registered error code from that input. Unknown codes collapse to a generic failure for that operation. The request has no field for that message, since an error string can contain a credential, provider response or file path.

Project only the following fields, with their provenance, into the snapshot.

| Source | Fields |
| --- | --- |
| Current Session's main branch, when readable | Up to 16 recent turn, model, tool and branch lifecycle records reduced to event kind, timestamp, sequence and registered outcome. Successful, failed and cancelled activity are eligible. |
| Current Session, only when redaction is turned off | Up to six recent user/assistant excerpts from the same main-branch cutoff, at most 2 KiB each and 8 KiB total. Only `report.md` includes them, as literal text; the diagnostic schema and sharing summary do not. |
| Captured error | Operation, registered error code, UTC observation time, fixed diagnostic summary |
| Error's referenced branch, when readable | The referenced terminal failure, time, verified correlation IDs and up to 16 nearby committed failure or lifecycle records reduced to event kind, timestamp, sequence and registered outcome |
| Process diagnostics | The daemon build's display label and protocol version, platform, the reporting client's label when it differs from the daemon's, and typed diagnostic metadata only when the owner supplies it for this error |
| Evidence status | Source and stable reason for missing or truncated evidence |

The Timeline read is branch-local and bounded by 32 candidate events ending at the captured head or the referenced failure. Validate that a failure reference resolves to a reportable failure in that branch. An observed-only error has no Timeline anchor. An unavailable reference, failed read or unavailable diagnostics source leaves the observed error intact. A successful Session read with zero eligible records is an empty excerpt. The process entry performs zero Session reads. Preparation reads published evidence without interrupting or rerunning work and uses only the admitted Session. [Timeline read ownership](../timeline-read-ownership.md), [observability boundaries](../../adr/005-observability.md#d1-two-observation-layers).

Diagnostics never include message bodies, raw logs, workspace files, tool input or output, or raw model requests. Their fields come from typed sources or fixed renderings, and correlation IDs come from verified metadata. With redaction on, remove correlation IDs and do not select conversation text. Only an explicit local plan with redaction off may add a recent excerpt to the Session report. Do not collect hidden reasoning, tool calls or results, attachments, instructions, or full history for the excerpt. User and assistant text can itself contain sensitive details; the user should inspect the ZIP before attaching it. Options maps recognized omit or keep instructions to field directives and shows unsupported instructions in the dialog. A keep directive cannot override redaction. The generation result cannot add new evidence.

## 4. Report methods

Register the Session namespace in `sessionMethods` and discover it via `session.methods`. The local error-only namespace is `report`, discovered as an admitted management contribution. Below, methods are suffixes of either namespace. Session calls include the ordinary `sessionId` required by the methods contract. Report calls require a live authenticated caller origin. [Session method contributions](../contracts/session-boundary.md#a1-method-contributions).

| Method | Inputs | Reply |
| --- | --- | --- |
| `prepare` | Session calls accept optional `error`. Omit it for a report on the admitted Session. When supplied, `error` is `{kind: "timeline", branchId, sequence, observed}` or `{kind: "observed", observed}`. The process part requires `error` in the observed variant. | `{reportId, defaultModel?, plan}`. Evidence availability is included in the plan. The default plan lets the dialog open with a preview without an extra round trip. |
| `plan` | `{reportId, generation, redact, instructions?}`, with generation `{kind: "local"}` or `{kind: "model", model: {providerId, modelId}}` | `{planId, generation, evidence, instructionsStatus}`. A model plan includes `material`, `maxOutputTokens` and `estimatedCost` as `{amount, currency}` or `null` for unknown. A successful replacement invalidates the previous plan. |
| `generate` | `{reportId, planId}` | `{title, body, files: {"report.md", "diagnostics.json"}, archive: {filename, base64}, sharing: {githubIssueUrl, discordUrl, description}}` |
| `cancel` | `{reportId, target: "generation", planId}` or `{reportId, target: "report"}` | `{cancelled: boolean}`. Generation cancellation affects only the named in-flight plan and keeps the evidence available for a new plan. An inactive plan returns `{cancelled: false}`. Report cancellation drops the draft. |

The TUI omits `error` for a Session report and sends the `observed` variant to the process entry for a conversation-open failure. Clients with an error-bound action, such as the Desktop and Web action in §2, can use the `timeline` variant to attach a known failure position in an admitted Session.

The local plan uses `generation: {kind: "local"}`. The `evidence` object includes the filtered diagnostic sources and a Timeline availability reason when activity records are absent. For a Session report, only a local plan with `redact: false` and Timeline enabled may also return `evidence.conversation` (an ordered array of `{role, sequence, text}`); the field is absent if no eligible text is available. `generate` renders that plan without reading later messages. Options recognizes comma- or newline-separated `omit` and `keep` directives for `timeline`, `error codes`, and `correlation ids`. The reply reports `instructionsStatus` as `none`, `applied`, `partially_applied`, or `unsupported`. It does not echo the entered text. Redaction always removes correlation IDs, so `keep correlation ids` is unsupported while redaction is on.

The observed error contains `operation` (`session_read`, `session_fork`, `branch_create` or `model_generation`), `code?`, `observedAt` (UTC timestamp) and `clientVersion` (the reporting client's build label, as the client's own `--version` prints it). No reader orders or compares it; it is display only. These fields carry neither Session authority nor a provider connection. The report ID and plan ID are opaque. Access requires the same caller connection and, for a Session draft, the same admitted Session. A missing, expired or foreign draft returns `report_not_found`. Invalid fields return `invalid_params`. A replaced or consumed plan returns `report_plan_stale`. A concurrent generation returns `report_busy`. A missing or changed model selection returns `report_model_unavailable` before a request. The other execution failures use `report_generation_failed` or `report_cancelled`. Error messages use stable summaries. Prepare returns a local plan when no usable model is available and retains the adopted model identity when known.

An immutable plan binds the exact material, selected model connection revision and output allowance. Generate atomically consumes the plan before starting work. Duplicate or late requests cannot resend it. Changing settings or retrying requires a new plan and a new explicit Generate. A model timeout or an ambiguous provider failure consumes the plan. Cancellation fences late replies, and a new generation waits for the old work to settle. Successful generation releases the draft and transfers the finished preview to the client. Closing the dialog or losing the connection releases any draft still open. The part cancels and settles accepted work on Session closing or unload using the existing call lifecycle and caller-closed signal. A bounded Session hold keeps the dialog alive between calls. [Part lifecycle](../plugin-contract.md#7-lifecycle-of-a-part), [call channels](../contracts/session-boundary.md#a2-call-channel), [activity holds](../appserver/idle-unload.md#51-activity-hold).

Use one draft per caller connection in each part instance, with a five-minute expiry and at most one generation in flight per draft. A new prepare replaces an idle draft for that connection. Allow up to 16 simultaneous drafts per part instance and return `report_busy` when full. Refuse replacement while a generation is settling. Bound the observed input and instructions to 1 KiB each and the prepared model material to 8 KiB. Bound model output to the lower of 1,024 tokens and the owner's allowance, with a 30-second generation deadline. Limit the encoded ZIP before constructing the RPC reply (§6). Expiry, connection loss, and close discard unsettled outputs. Drafts are ephemeral and have no on-disk store.

## 5. Generation

Local generation renders a template from the approved snapshot and uses zero model calls. The optional short-lived report Agent performs one isolated drafting operation using only the reviewed material. Its input is the fixed system instruction, filtered snapshot and resolved redaction directives. It has no inherited Session, tools, workspace, credentials or publishing access. The Provider owner resolves and authenticates the request. The report plan shows the exact prompt material, the chosen provider/model, output limit and an estimate when the necessary price and token count are available. A plan cannot silently fall back to a different connection, model or request payload.

The Agent's operation uses the one-shot auxiliary model boundary, with the single-attempt policy requested in §7. It does not create a general-purpose chat Session. The model proposes the title template, body section order and evidence selections and omissions using a bounded schema of known fields. Report validates the selection and renders the actual text from approved values. Invalid output fails generation and offers the local template. Neither path includes reproduction steps unless approved evidence establishes them.

## 6. Preview and sharing

Build `report.md` and versioned `diagnostics.json` from the same approved content used for the report. The `Dim` line names the daemon build's display label and protocol version, and a `Client` line names the reporting client's label when it differs from the daemon's: a CLI talking to a daemon of another build is exactly what a report must reveal. Error reports use diagnostics v1 (`{version: 1, evidence, unavailable}`), with `evidence.capturedError` and any referenced failure evidence. Reports on the current Session use diagnostics v2 (`{version: 2, evidence, unavailable}`), with `evidence.process` and, when included, `evidence.timeline` containing `recent` records and `truncated`. The generated title is `Session report`. The ZIP contains exactly those two files. Return the two UTF-8 file contents and the base64-encoded ZIP in the direct reply so clients can save after the connection closes. Cap the ZIP at 64 KiB before encoding. The TUI exposes share actions after the ZIP has been saved and the user can open it. It leaves the saved path in the transcript when the dialog closes. Clients with inline previews display the body and both file contents before exposing share actions and treat them as data when rendering them.

| Action | Client behavior |
| --- | --- |
| Save ZIP | Save or download the received bytes on the client device. Show the resulting path where the platform exposes it. |
| Draft a GitHub issue | Open an issue draft on `arcships/dim` with a diagnostic title and summary only. The user decides whether to attach the ZIP and submit the issue. |
| Give feedback on Discord | Open the [Dim Discord invite](https://discord.gg/ds35K3TadP) and copy the diagnostic summary only. The user chooses whether and where to post it or attach the ZIP. |

After a successful save, both TUI modes show a hint below the saved path. Users can attach the saved ZIP to an email to `feedback@arcships.ai`.

The client keeps the completed preview and bytes when a share action fails. The plugin cannot publish an issue or upload an archive. The browser flow is sufficient without an authenticated `gh`. [GitHub's attachment flow](https://docs.github.com/en/get-started/writing-on-github/working-with-advanced-formatting/attaching-files).

## 7. Requirements raised to mechanism owners

| Blocker and existing alternatives | Required capability and ordering | Owner |
| --- | --- | --- |
| Session contribution admission requires a loaded Session. A conversation read may fail before that load completes. Local management contributions work outside a Session, but Web exposes only explicitly admitted names and paired entries serve only Session methods. | Admit the error-only methods on the authenticated local Web entry via its explicit allowlist. A paired error-only path needs an agreed authenticated admission contract for narrowly scoped client operations without a Session. Reject such calls under the current paired contract. The entry grants no Session access, adds no facts, and keeps existing disconnect and reload semantics. | App Server Web entry and Rpc owners, [#1319](https://github.com/arcships/dim/issues/1319). [Web admission](../web-workspace.md#23-http-surface), [paired entry](RemoteGateway.md). |
| The auxiliary call accepts one request from its consumer but invokes the shared retry and credential-recovery policy underneath. A report cannot enforce its request budget from the caller side. | Offer a caller-selectable single-attempt policy through the auxiliary boundary, including its provider transport. The owner enforces the prepared target revision and never retries or substitutes after a send. Preserve cancellation, reload settlement and other callers' policy. Enable model-assisted reports only where the owner can honor this contract. | Auxiliary model and provider owners, [#1319](https://github.com/arcships/dim/issues/1319). [Auxiliary model interface](../appserver/auxiliary-model.md#4-interfaces), [model retry policy](../model-failures.md). |
| The curated TUI list has no `/report` slot. A hidden command would leave the documented command discoverability incomplete. | The CLI command owner adds the menu entry with the order and behavior in §2. This is client-side and does not affect Timeline ordering, interruption or reload. | CLI command owner, [#1319](https://github.com/arcships/dim/issues/1319). [Command menu](../cli-commands.md#command-menu). |

## 8. Dependencies and rule check

The Session part declares the `sessionMethods`, `sessionLifecycle` and `treeCommands` data faces. It uses the [public branch read surface](../timeline-read-ownership.md#8-binding-appendix) for the published cutoff and adopted model selection. The process part declares the `managementMethods` and `partRegistry` data faces. Both use caller origin and the process build metadata already available to App Server. With no approved typed diagnostics source, build and platform information are sufficient process evidence. Model-assisted generation depends on the model owners in §7. App Server composes the parts as one feature. [Service catalog](../../architecture/rust-agent/generated/catalog.md).

| Rule | Check |
| --- | --- |
| Principles §1 and ADR 002 D1 | Report has a Session part and a process part for its respective draft scopes. Neither is a Capability. They read a bounded snapshot and do not fold or write Timeline state. |
| §4 rules 1 and 2 | Neither part commands another state owner or hands off an object through the Timeline. The client explicitly submits an immutable plan to its owner. |
| §4 rules 3 and 5 | No before callback or event batch applies. |
| §4 rules 4 and 8 | Report uses generic method admission, public reads and lifecycle. Owner changes needed for Web admission and model policy are listed in §7. |
| §4 rule 6 | Source context is untouched. The isolated operation uses only reviewed material. |
| §4 rule 7 and ADR 002 D3 | Dependencies point to public read, lifecycle, and model services. Report reads no private storage or provider state. |
| §4 rule 9 | No Timeline events or broadcast notices are added. Results go to the originating RPC caller. |
| ADR 002 D2 | The feature owns the report work and state. Its parts register their own methods. |
| ADR 002 D4 and D5 | Per-report settings live in the draft. The RPC contract is language-neutral. |

## 9. Verification

- Exercise `/report` in both TUI modes with an error-free Session, during a running turn, and after switching from a Session with an earlier error. Verify that the report stays bound to the Session selected when it opened and that a fresh command targets the newly shown Session. Cover Home with and without a failed conversation open.
- Generate a ZIP from a Session with successful and repeated activity. Verify the excerpt comes from the main branch at the captured head, excludes later activity, respects the window and record bounds, and supports an empty excerpt. Verify both archive versions through their corresponding prepare inputs.
- Exercise a failed authorized history read and failed Session admission separately. Confirm the process route does not read Session data, the local Web route requires authentication, and guessed, foreign or stale report IDs do not expose drafts.
- Seed distinct markers in public conversation text, hidden fields, raw tool output and source errors. Verify no marker reaches the default ZIP; only the opted-in user and assistant text appears in `report.md`, never in `diagnostics.json`, the GitHub URL or the Discord description. Turn redaction back on and verify the new plan omits the excerpt. Compare both returned preview files byte-for-byte with the archive entries.
- Use a counting provider to verify zero local requests, one model request after Generate, and no retry after a failed or duplicate call. Verify cancellation, expiry, close and reconnect settle work, discard late output and allow local generation from the same evidence while the draft remains open. A late cancellation for a settled plan leaves its successor running.
- Generate a report on a daemon and confirm its `Dim` line carries the same label the binary prints, and that a client whose `clientVersion` names another build adds one `Client` line, in the body and in `evidence.process.clientVersion`. `crates/dim-report/tests/report_flow.rs` covers this at RPC level; `crates/dimcode/tests/cli_flow/report.rs` covers the product daemon and binary.
- Save a report ZIP on a client connected to another host and exercise the GitHub draft and Discord flows without `gh`. Verify explicit sharing follows review or local save and that a failed share leaves the saved bytes available.

## 10. Ablation

The Session namespace serves current-Session reports through the existing prepare method. The process entry is needed for a conversation-open error that fails Session admission and uses the same flow with fewer evidence sources. The plan ID ties a paid request to the exact preview the user confirmed. The bounded ZIP in the generation reply lets the client save without a second download resource or archive store. An isolated one-shot model operation supplies drafting and redaction without a general-purpose report Session.
