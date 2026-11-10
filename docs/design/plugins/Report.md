# Report plugin

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

The Session part owns the draft and reads only the admitted Session. If that Session opens but its history read fails, the same draft proceeds with the observed error and an explicit missing-evidence reason. If Session admission itself fails, the Session handler is unreachable. The process part serves an error-only draft using the same flow without accepting a Session or branch ID. It owns the draft because there may be no loaded Session.

The error-only entry uses `managementMethods` on the local App Server connection and requires explicit admission on the authenticated local Web entry (§7). Paired connections can use `session.report.*` while Session admission succeeds. The paired error-only entry is a separate admission decision in §7. Preparing a report requires a live connection. A TUI that already received the generated ZIP can still save it after the connection closes.

## 2. Client interaction

Both TUI modes display the hint below a reportable error, including a failed conversation read. `/report` selects the most recent error for which that TUI displayed the hint and retains the error's original Session and branch reference, if present. A Session switch never rebinds that error. With no reportable error, the command says so. Desktop and Web bind the action to the selected error. A failure of the report flow itself stays inside the dialog.

```text
! Could not read conversation state
  Run /report to report this error.

+-- Report this error -----------------------------------------+
| Model      provider/model (current)              [Change]    |
| Redact     [x]                                  [Options]    |
| Evidence   Error captured. Conversation evidence missing.    |
| Model send [Review material]       Up to 1,024 output tokens |
| Estimated cost unknown                                       |
| [Generate report]                                 [Cancel]   |
+--------------------------------------------------------------+

+-- Report ready ----------------------------------------------+
| <Report title>                                               |
| ZIP contents: report.md, diagnostics.json                    |
| > Save ZIP                                                   |
|   Close                                                      |
+--------------------------------------------------------------+
  After saving: <ZIP path> · GitHub · Discord
```

Local generation is available on its own. Clients can expose a model picker once the one-shot operation in §7 is available. Its initial model is the adopted model of the referenced branch, or of the Session's main branch for an observed-only report. It also offers models from the client's admitted catalog and **Generate locally**. Redaction starts checked. Options accepts a short instruction about what to omit or keep. There are no required fields. With a model selected, the dialog exposes the exact model material and limits before Generate can send it. Local mode displays zero model requests. After a stale plan or a failed generation, the client offers to refresh the plan. When the draft expires, the client offers to prepare a report from the original error and reapplies the chosen options.

The overlay follows the TUI's focus and list conventions. Up and Down move between controls, and Tab and Shift+Tab cycle focus. Enter activates the focused control. Esc closes a picker first, cancels generation while busy, or closes the report. Focus starts on Model. The TUI shortens the error and report title to fit the overlay and keeps the focused action visible. The result lists the archive contents and shows the saved path so users can inspect the files with their own tools. Place `/report` immediately before `/exit` in both curated menus, following the existing exact-name, Enter and Tab behavior. The CLI command owner owns that order change (§7). [TUI design](Tui.md), [command menu contract](../cli-commands.md#command-menu).

## 3. Evidence and redaction

The client supplies either a typed Timeline failure reference with an observed-error fallback, or an observed error alone. A typed reference contains the branch and committed sequence in the admitted Session. An observed error contains a registered client operation, a stable error code if present, the observation time and the client version. The client keeps the displayed message for the dialog. The report projects a stable operation summary and registered error code from that input. Unknown codes collapse to a generic failure for that operation. The request has no field for that message, since an error string can contain a credential, provider response or file path.

Project only the following fields, with their provenance, into the snapshot.

| Source | Fields |
| --- | --- |
| Captured error | Operation, registered error code, UTC observation time, fixed diagnostic summary |
| Authorized branch, when readable | The referenced terminal failure, time, verified correlation IDs and up to 16 nearby committed failure or lifecycle records reduced to event kind, timestamp, sequence and registered outcome |
| Process diagnostics | Product and protocol versions, platform, and typed diagnostic metadata only when the owner supplies it for this error |
| Evidence status | Source and stable reason for missing or truncated evidence |

The Timeline read is branch-local and bounded by 32 candidate events around the reference. Validate that the reference resolves to a reportable failure in that branch. An observed-only error has no Timeline anchor. An unavailable reference, failed read or unavailable diagnostics source leaves the observed error intact. The process entry performs zero Session reads. The report neither repeats the failed operation nor searches other Sessions. It reads through the existing public owners and applies the two observation layers. [Timeline read ownership](../timeline-read-ownership.md), [observability boundaries](../../adr/005-observability.md#d1-two-observation-layers).

Project fields before applying the optional redaction setting. Reject arbitrary nested data. Raw log lines, messages, full history, workspace files, paths, tool input and output, raw requests and credentials have no fields in the export schema. Each approved string comes from a typed source or a fixed rendering, and correlation IDs come from verified metadata. Redaction on removes correlation IDs and any deployment-specific labels. Options maps recognized omit or keep instructions to safe field directives, shows unsupported instructions in the dialog, and sends only recognized directives. Keep directives cannot restore forbidden data. Model-selected semantic omissions can further reduce the approved fields. The generation result cannot add new evidence.

## 4. Report methods

Register the Session namespace in `sessionMethods` and discover it via `session.methods`. The local error-only namespace is `report`, discovered as an admitted management contribution. Below, methods are suffixes of either namespace. Session calls include the ordinary `sessionId` required by the methods contract. Report calls require a live authenticated caller origin. [Session method contributions](../contracts/session-boundary.md#a1-method-contributions).

| Method | Inputs | Reply |
| --- | --- | --- |
| `prepare` | `{error}`. Session error is `{kind: "timeline", branchId, sequence, observed}` or `{kind: "observed", observed}`. The process part accepts only the observed variant. | `{reportId, defaultModel?, plan}`. Evidence availability is included in the plan. The default plan lets the dialog open with a preview without an extra round trip. |
| `plan` | `{reportId, generation, redact, instructions?}`, with generation `{kind: "local"}` or `{kind: "model", model: {providerId, modelId}}` | `{planId, generation, evidence, instructionsStatus}`. A model plan includes `material`, `maxOutputTokens` and `estimatedCost` as `{amount, currency}` or `null` for unknown. A successful replacement invalidates the previous plan. |
| `generate` | `{reportId, planId}` | `{title, body, files: {"report.md", "diagnostics.json"}, archive: {filename, base64}, sharing: {githubIssueUrl, discordUrl, description}}` |
| `cancel` | `{reportId, target: "generation", planId}` or `{reportId, target: "report"}` | `{cancelled: boolean}`. Generation cancellation affects only the named in-flight plan and keeps the evidence available for a new plan. An inactive plan returns `{cancelled: false}`. Report cancellation drops the draft. |

The local plan uses `generation: {kind: "local"}`. The `evidence` object includes the filtered sources and a Timeline availability reason when the excerpt is absent. Options recognizes comma- or newline-separated `omit` and `keep` directives for `timeline`, `error codes`, and `correlation ids`. The reply reports `instructionsStatus` as `none`, `applied`, `partially_applied`, or `unsupported`. It does not echo the entered text. Redaction always removes correlation IDs, so `keep correlation ids` is unsupported while redaction is on.

The observed error contains `operation` (`session_read`, `session_fork`, `branch_create` or `model_generation`), `code?`, `observedAt` (UTC timestamp) and `clientVersion` (build version). These fields carry neither Session authority nor a provider connection. The report ID and plan ID are opaque. Access requires the same caller connection and, for a Session draft, the same admitted Session. A missing, expired or foreign draft returns `report_not_found`. Invalid fields return `invalid_params`. A replaced or consumed plan returns `report_plan_stale`. A concurrent generation returns `report_busy`. A missing or changed model selection returns `report_model_unavailable` before a request. The other execution failures use `report_generation_failed` or `report_cancelled`. Error messages use stable summaries. Prepare returns a local plan when no usable model is available and retains the adopted model identity when known.

An immutable plan binds the exact material, selected model connection revision and output allowance. Generate atomically consumes the plan before starting work. Duplicate or late requests cannot resend it. Changing settings or retrying requires a new plan and a new explicit Generate. A model timeout or an ambiguous provider failure consumes the plan. Cancellation fences late replies, and a new generation waits for the old work to settle. Successful generation releases the draft and transfers the finished preview to the client. Closing the dialog or losing the connection releases any draft still open. The part cancels and settles accepted work on Session closing or unload using the existing call lifecycle and caller-closed signal. A bounded Session hold keeps the dialog alive between calls. [Part lifecycle](../plugin-contract.md#7-lifecycle-of-a-part), [call channels](../contracts/session-boundary.md#a2-call-channel), [activity holds](../appserver/idle-unload.md#51-activity-hold).

Use one draft per caller connection in each part instance, with a five-minute expiry and at most one generation in flight per draft. A new prepare replaces an idle draft for that connection. Allow up to 16 simultaneous drafts per part instance and return `report_busy` when full. Refuse replacement while a generation is settling. Bound the observed input and instructions to 1 KiB each and the prepared model material to 8 KiB. Bound model output to the lower of 1,024 tokens and the owner's allowance, with a 30-second generation deadline. Limit the encoded ZIP before constructing the RPC reply (§6). Expiry, connection loss, and close discard unsettled outputs. Drafts are ephemeral and have no on-disk store.

## 5. Generation

Local generation renders a template from the approved snapshot and uses zero model calls. The optional short-lived report Agent performs one isolated drafting operation using only the reviewed material. Its input is the fixed system instruction, filtered snapshot and resolved redaction directives. It has no inherited Session, tools, workspace, credentials or publishing access. The Provider owner resolves and authenticates the request. The report plan shows the exact prompt material, the chosen provider/model, output limit and an estimate when the necessary price and token count are available. A plan cannot silently fall back to a different connection, model or request payload.

The Agent's operation uses the one-shot auxiliary model boundary, with the single-attempt policy requested in §7. It does not create a general-purpose chat Session. The model proposes the title template, body section order and evidence selections and omissions using a bounded schema of known fields. Report validates the selection and renders the actual text from approved values. Invalid output fails generation and offers the local template. Neither path includes reproduction steps unless approved evidence establishes them.

## 6. Preview and sharing

Build `report.md` and versioned `diagnostics.json` from the same approved content used for the report. Diagnostics v1 contains `{version: 1, evidence, unavailable}`. The ZIP contains exactly those two files. Return the two UTF-8 file contents and the base64-encoded ZIP in the direct reply so clients can save after the connection closes. Cap the ZIP at 64 KiB before encoding. The TUI exposes share actions after the ZIP has been saved and the user can open it. It leaves the saved path in the transcript when the dialog closes. Clients with inline previews display the body and both file contents before exposing share actions and treat them as data when rendering them.

| Action | Client behavior |
| --- | --- |
| Save ZIP | Save or download the received bytes on the client device. Show the resulting path where the platform exposes it. |
| GitHub | Open an issue draft on `arcships/dim` with the report title and body. Supply the same ZIP for manual attachment. |
| Discord | Offer the ZIP, copyable description and the [Dim Discord invite](https://discord.gg/ds35K3TadP). |

The client keeps the completed preview and bytes when a share action fails. The plugin cannot publish an issue or upload an archive. The browser flow is sufficient without an authenticated `gh`. [GitHub's attachment flow](https://docs.github.com/en/get-started/writing-on-github/working-with-advanced-formatting/attaching-files).

## 7. Requirements raised to mechanism owners

| Blocker and existing alternatives | Required capability and ordering | Owner |
| --- | --- | --- |
| Session contribution admission requires a loaded Session. A conversation read may fail before that load completes. Local management contributions work outside a Session, but Web exposes only explicitly admitted names and paired entries serve only Session methods. | Admit the error-only methods on the authenticated local Web entry via its explicit allowlist. A paired error-only path needs an agreed authenticated admission contract for narrowly scoped client operations without a Session. Reject such calls under the current paired contract. The entry grants no Session access, adds no facts, and keeps existing disconnect and reload semantics. | App Server Web entry and Rpc owners, [#1319](https://github.com/arcships/dim/issues/1319). [Web admission](../web-workspace.md#23-http-surface), [paired entry](RemoteGateway.md). |
| The auxiliary call accepts one request from its consumer but invokes the shared retry and credential-recovery policy underneath. A report cannot enforce its request budget from the caller side. | Offer a caller-selectable single-attempt policy through the auxiliary boundary, including its provider transport. The owner enforces the prepared target revision and never retries or substitutes after a send. Preserve cancellation, reload settlement and other callers' policy. Enable model-assisted reports only where the owner can honor this contract. | Auxiliary model and provider owners, [#1319](https://github.com/arcships/dim/issues/1319). [Auxiliary model interface](../appserver/auxiliary-model.md#4-interfaces), [model retry policy](../model-failures.md). |
| The curated TUI list has no `/report` slot. A hidden command would leave the documented command discoverability incomplete. | The CLI command owner adds the menu entry with the order and behavior in §2. This is client-side and does not affect Timeline ordering, interruption or reload. | CLI command owner, [#1319](https://github.com/arcships/dim/issues/1319). [Command menu](../cli-commands.md#command-menu). |

## 8. Dependencies and rule check

The Session part declares `sessionMethods`, Session lifecycle, public Session/branch reads for evidence and adopted model selection, and the optional model catalog and auxiliary model owner. The process part declares `managementMethods` and optional model services. Both use caller origin and the process build metadata already available to App Server. With no approved typed diagnostics source, build and platform information are sufficient process evidence. App Server composes the parts as one feature.

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

- Exercise the same error, keyboard flow and no-error state in both TUI renderers, including an unreadable conversation. Verify that an error remains bound to its original Session and branch after switching views.
- Exercise a failed authorized history read and failed Session admission separately. Confirm the process route does not read Session data, the local Web route requires authentication, and guessed, foreign or stale report IDs do not expose drafts.
- Seed forbidden fields and free-text secrets into source errors and evidence. Assert their absence from the model material and both ZIP members under either redaction setting. Compare the returned preview files byte-for-byte with the archive entries.
- Use a counting provider to verify zero local requests, one model request after Generate, and no retry after a failed or duplicate call. Verify cancellation, expiry, close and reconnect settle work, discard late output and allow local generation from the same evidence while the draft remains open. A late cancellation for a settled plan leaves its successor running.
- Save a report ZIP on a client connected to another host and exercise the GitHub draft and Discord flows without `gh`. Verify explicit sharing follows review or local save and that a failed share leaves the saved bytes available.

## 10. Ablation

The Session namespace alone cannot serve an error that fails Session admission. The process entry uses the same flow with fewer evidence sources and does not restore a Session. The plan ID is needed to tie a paid request to the exact preview the user confirmed. Returning the bounded ZIP in the generation reply avoids a second download resource or an archive store. An isolated one-shot model operation supplies drafting and redaction without a general-purpose report Session.
