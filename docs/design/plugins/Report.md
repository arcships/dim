# Report

Scope: process and Session. Issue: [#1319](https://github.com/arcships/dim/issues/1319), contract #2021.

Report owns the drafts behind `/report` and the error-only report a client opens when no Session can be read. This document is the owner's target contract; the client interaction and the evidence rules below are the same design, restated in the contract's shape. The TimelineTree, SessionScope, ManagementMethods and SessionMethods owners are separate; their operations are dependencies.

## Use and scope: responsibility split

Report owns:

- **Drafts.** One per calling connection in each part instance: the captured snapshot, the plan, at most one generation in flight, a five-minute expiry, and the Session hold that keeps the dialog's Session loaded.
- **Evidence.** Which records of the admitted Session or the observed error are selected, filtered, bounded and redacted, and what the caller sees before a plan.
- **Plans and generation.** The immutable plan, its consumption, one-shot generation, cancellation and the settlement of accepted work.
- **The archive.** `report.md`, `diagnostics.json` and the bounded ZIP returned to the client.

Callers keep:

- The error hint or action, confirmation and local save or sharing: the client owns all three.
- Session admission: Rpc and Agents decide whether the Session can be read; Report reads only the admitted Session.
- The model's transport and retry policy: the Provider and auxiliary-model owners.
- The TUI menu entry: the CLI command owner.

| Requirement (callers) | Can callers compose it from the operations below without breaking the owner's invariants? | Handling |
| --- | --- | --- |
| A client prepares a report on the Session it views | No: the draft, its expiry, its hold and the captured cutoff are the owner's | `prepare` (Session namespace) |
| A client on Home reports a conversation-open failure | No: the process part has no Session and drafts from the observed error | `prepare` (process namespace) |
| A client reviews the exact material and chooses redaction and instructions | No: an immutable plan is the owner's state | `plan` |
| A client generates and receives the preview and the ZIP | No: plan consumption, one-shot generation and the archive are the owner's | `generate` |
| A client cancels a plan or the draft | No: only the owner can stop its generation and discard the draft | `cancel` |

An operation or parameter exists only where the row says No.

## Caller example

The TUI's `/report` flow:

```rust
let prepared = report.prepare(PrepareRequest { error: None }).await?; // Session namespace
let plan = report.plan(PlanRequest {
    report_id: prepared.report_id,
    generation: Generation::Local,
    redact: true,
    instructions: None,
}).await?; // the exact material and the directives, reviewed before Generate
let report = report.generate(GenerateRequest { report_id, plan_id: plan.plan_id }).await?;
save(report.archive); // the client saves after the connection may close
```

Home's error-only flow uses the same three calls in the process namespace with `error` present.
## Client interaction

In both TUI modes, `/report` opens a report for the current Session, including while a turn is running. It needs no captured error. Users can report an unsatisfactory answer or a repeating turn without waiting for a failure or stopping the turn. The dialog keeps the Session selected when it opened, including when preparing again after expiry. Returning to the composer and running `/report` again selects the Session shown at that time.

Both modes show `Run /report to create a report.` below reportable errors. When a Session is shown, the hint points to the Session report and its recent activity excerpt (see [Evidence and redaction](#evidence-and-redaction)). On Home, the command can report a conversation-open failure from that view through the process entry. Leaving a Session clears its eligibility as an error source for Home. With neither a Session nor a conversation-open failure to report, the command shows `No session to report.` Desktop and Web error actions remain bound to the selected error. A failure of the report flow stays inside the dialog.

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

The error-only overlay uses `Report this error`. The Session overlay shows when the Timeline could not be read or the bounded excerpt has no reportable activity. Either state permits generation. Local generation is available on its own. Clients can expose a model picker once the Provider's one-shot operation is available (see [Requirements raised to mechanism owners](#requirements-raised-to-mechanism-owners)). Its initial model is the adopted model of the referenced branch, or of the Session's main branch when there is no branch reference. It also offers models from the client's admitted catalog and **Generate locally**. Redaction starts checked on every new report: Generate then Save ZIP produces diagnostics only. Before generation, the Session dialog explains that turning redaction off may include sensitive text from recent user and assistant messages. An error-only report has no conversation to attach. Generating and saving never send a report to a feedback destination. Options accepts a short instruction about what to omit or keep. There are no required fields. With a model selected, the dialog exposes the exact model material and limits before Generate can send it. Local mode displays zero model requests. After a stale plan or a failed generation, the client offers to refresh the plan. When the draft expires, the client offers to prepare a report for the original target and reapplies the chosen options.

The overlay follows the TUI's focus and list conventions. Up and Down move between controls, and Tab and Shift+Tab cycle focus. Enter activates the focused control. Esc closes a picker first, cancels generation while busy, or closes the report. Focus starts on Model. The TUI shortens the error and report title to fit the overlay and keeps the focused action visible. The result lists the archive contents and shows the saved path so users can inspect the files with their own tools. Place `/report` immediately before `/exit` in both curated menus with the description `report the current session`, following the existing exact-name, Enter and Tab behavior. The CLI command owner owns the menu. [TUI design](Tui.md), [command menu contract](../cli-commands.md#command-menu).

## Evidence and redaction

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

## Own contract

### Dependencies

TimelineTree's bounded branch read and branch summary ([#2024](https://github.com/arcships/dim/issues/2024)); SessionScope's hold and closing ([#2025](https://github.com/arcships/dim/issues/2025)); ManagementMethods' `register` (process namespace) and the Session method table's registration; the process build label and protocol version for diagnostics.

### Operations

| Operation | Intent | Request | Reply | Cancellation | Errors |
| --- | --- | --- | --- | --- | --- |
| `prepare` | Capture a bounded snapshot and open a draft | `{error?}`; the process namespace requires `error` in the observed variant | `{reportId, defaultModel?, plan}` | Before the snapshot is captured; nothing is drafted | `caller_required`, `invalid_params`, `report_cancelled` |
| `plan` | Bind the exact material, generation and directives | `{reportId, generation, redact, instructions?}` | `{planId, generation, evidence, instructionsStatus, material, maxOutputTokens, estimatedCost}` | None; the plan is a value | `report_not_found`, `invalid_params`, `report_busy`, `report_model_unavailable` |
| `generate` | Consume the plan and produce the archive | `{reportId, planId}` | `{title, body, files, archive, sharing}` | The named plan stops; the evidence stays for a new plan | `report_not_found`, `report_plan_stale`, `report_busy`, `report_generation_failed`, `report_cancelled` |
| `cancel` | Stop a generation or drop a draft | `{reportId, target: "generation", planId}` or `{reportId, target: "report"}` | `{cancelled}` | — | `report_not_found` |

The draft is keyed by the calling connection and the part instance; a missing, expired or foreign draft is `report_not_found`. A new `prepare` replaces an idle draft for that connection. At most 16 drafts per part instance; a seventeenth is `report_busy`. The observed input and instructions are bounded to 1 KiB each; the prepared model material to 8 KiB; the encoded ZIP to 64 KiB before the reply.

### Errors

| Kind | What the caller does next |
| --- | --- |
| `caller_required` | The method needs a client connection; an in-process caller cannot prepare a report. Never reaches a client |
| `invalid_params` | Fix the request: a missing or malformed error reference, a model id outside the admitted catalog, an out-of-range field |
| `report_not_found` | The draft expired or belongs to another connection; prepare again |
| `report_plan_stale` | The plan was replaced or consumed; review the new plan before generating |
| `report_busy` | A generation is running for this draft; cancel it or wait for it to settle |
| `report_model_unavailable` | The selected model cannot serve this request; choose another or generate locally |
| `report_generation_failed` | The generation failed; the draft keeps its evidence and the caller retries with a new plan or generates locally |
| `report_cancelled` | The generation stopped; the draft keeps its evidence |

### Flows

| Operation | Normal path | Caller cancels | Caller unloads | Owner closes or reloads | Settled by |
| --- | --- | --- | --- | --- | --- |
| `prepare` | Capture the snapshot at the published cutoff within the bounds, take the Session hold, open the draft | The snapshot read stops; no draft | The draft is discarded with the part | Every draft is discarded and its hold released | Report: the draft and its hold |
| `plan` | Validate the request, compute the reviewed evidence, replace the previous plan | — | The draft is discarded | The plan is discarded with the draft | Report |
| `generate` | Consume the plan atomically, render or run the one-shot model call, return the archive | The named plan stops; late output is discarded; the evidence stays | The draft and its work are discarded | Accepted work settles; late output is discarded | Report |
| `cancel` | Stop the named generation or drop the draft | — | — | — | Report |


## Exceptions with reasons

- The process namespace accepts no Session or branch id: an error-only report must work when Session admission failed, so the part has no Session to name.
- The archive is returned in the `generate` reply: the client must be able to save after the connection closes, and a second download resource would be another owner's store.
- Evidence is read at the published cutoff and rendered later: the snapshot is a projection Report owns, so a later turn cannot change a reviewed plan.

## Generation

Local generation renders a template from the approved snapshot and uses zero model calls. The optional short-lived report Agent performs one isolated drafting operation using only the reviewed material. Its input is the fixed system instruction, filtered snapshot and resolved redaction directives. It has no inherited Session, tools, workspace, credentials or publishing access. The Provider owner resolves and authenticates the request. The report plan shows the exact prompt material, the chosen provider/model, output limit and an estimate when the necessary price and token count are available. A plan cannot silently fall back to a different connection, model or request payload.

The Agent's operation uses the one-shot auxiliary model boundary, with the single-attempt policy requested from the Provider (see [Requirements raised to mechanism owners](#requirements-raised-to-mechanism-owners)). It does not create a general-purpose chat Session. The model proposes the title template, body section order and evidence selections and omissions using a bounded schema of known fields. Report validates the selection and renders the actual text from approved values. Invalid output fails generation and offers the local template. Neither path includes reproduction steps unless approved evidence establishes them.

## Preview and sharing

Build `report.md` and versioned `diagnostics.json` from the same approved content used for the report. The `Dim` line names the daemon build's display label and protocol version, and a `Client` line names the reporting client's label when it differs from the daemon's: a CLI talking to a daemon of another build is exactly what a report must reveal. Error reports use diagnostics v1 (`{version: 1, evidence, unavailable}`), with `evidence.capturedError` and any referenced failure evidence. Reports on the current Session use diagnostics v2 (`{version: 2, evidence, unavailable}`), with `evidence.process` and, when included, `evidence.timeline` containing `recent` records and `truncated`. The generated title is `Session report`. The ZIP contains exactly those two files. Return the two UTF-8 file contents and the base64-encoded ZIP in the direct reply so clients can save after the connection closes. Cap the ZIP at 64 KiB before encoding. The TUI exposes share actions after the ZIP has been saved and the user can open it. It leaves the saved path in the transcript when the dialog closes. Clients with inline previews display the body and both file contents before exposing share actions and treat them as data when rendering them.

| Action | Client behavior |
| --- | --- |
| Save ZIP | Save or download the received bytes on the client device. Show the resulting path where the platform exposes it. |
| Draft a GitHub issue | Open an issue draft on `arcships/dim` with a diagnostic title and summary only. The user decides whether to attach the ZIP and submit the issue. |
| Give feedback on Discord | Open the [Dim Discord invite](https://discord.gg/ds35K3TadP) and copy the diagnostic summary only. The user chooses whether and where to post it or attach the ZIP. |

After a successful save, both TUI modes show a hint below the saved path. Users can attach the saved ZIP to an email to `feedback@arcships.ai`.

The client keeps the completed preview and bytes when a share action fails. The plugin cannot publish an issue or upload an archive. The browser flow is sufficient without an authenticated `gh`. [GitHub's attachment flow](https://docs.github.com/en/get-started/writing-on-github/working-with-advanced-formatting/attaching-files).

## Requirements raised to mechanism owners

| Owner | Required operation or value | Why the caller cannot compose it |
| --- | --- | --- |
| TimelineTree ([#2024](https://github.com/arcships/dim/issues/2024)) | A bounded branch read `{branchId, through, kinds, limit} → {events, truncated}` and a branch summary `{head, adoptedModel}`; the default branch id | The read must stop at a committed cutoff and stay inside the owner's pages; composing it from raw queries would let the caller re-fold branch state |
| SessionScope ([#2025](https://github.com/arcships/dim/issues/2025)) | `hold() → holdId`, `release(holdId)`, `closing()` | The hold keeps the Session loaded while the dialog is open; its settlement is the Session owner's |
| Session registration ([#2026](https://github.com/arcships/dim/issues/2026)) | The Session method table's `call` request carries `{method, params, caller: {connectionId, callId}}` | Report binds its draft to the calling connection; the caller identity is data |
| Provider ([#2019](https://github.com/arcships/dim/issues/2019)) | A one-shot completion with a caller-selected no-retry policy and the exact prepared material | Model-assisted generation is outside this contract until the owner offers that operation; the local template stays the default |
| Web entry (App Server, this block) | Admit the process `report.*` namespace on the authenticated local Web entry | The error-only entry is served to the web client; the admission list is the entry's |

## Supersedes

None. This document replaces its own previous ownership and methods sections in place; the client interaction, evidence and generation sections stay.

## Rule check

The checks below cite [design rules §7, PR #2029](https://github.com/arcships/dim/pull/2029) because the section is not yet on `main`.


| Rule | Application |
| --- | --- |
| Principles §1 and ADR 002 D1 | Report has a Session part and a process part for their draft scopes; neither is a Capability. |
| §2 scopes | The Session part drafts for its Session; the process part drafts for errors without a readable Session. |
| §3 orthogonality | No other owner's state is read; TimelineTree, SessionScope and the method tables are asked through their operations. |
| §4 rules 1 and 2 | Neither part commands another state owner or hands off an object through the Timeline; the client submits an immutable plan. |
| §4 rules 3 and 5 | No before callback or event batch applies. |
| §4 rules 4 and 8 | Report uses generic method admission, public reads and lifecycle; the requirements above are the only owner changes. |
| §4 rule 6 | Source context is untouched; the one-shot operation uses only reviewed material. |
| §4 rule 7 and ADR 002 D3 | Dependencies point to public read, lifecycle and model services; no private storage or provider state is read. |
| §4 rule 9 | No Timeline events or broadcast notices; results go to the originating caller. |
| §7 no handle | The draft, the plan and the ZIP are values addressed by ids; the Session hold is an id. |
| ADR 002 D2 | The feature owns the report work and state; its parts register their own methods. |
| ADR 002 D4 and D5 | Per-report options live in the draft; the RPC contract is language-neutral. |

## Verification

- Exercise `/report` in both TUI modes with an error-free Session, during a running turn, and after switching from a Session with an earlier error. Verify that the report stays bound to the Session selected when it opened and that a fresh command targets the newly shown Session. Cover Home with and without a failed conversation open.
- Generate a ZIP from a Session with successful and repeated activity. Verify the excerpt comes from the main branch at the captured head, excludes later activity, respects the window and record bounds, and supports an empty excerpt. Verify both archive versions through their corresponding prepare inputs.
- Exercise a failed authorized history read and failed Session admission separately. Confirm the process route does not read Session data, the local Web route requires authentication, and guessed, foreign or stale report IDs do not expose drafts.
- Seed distinct markers in public conversation text, hidden fields, raw tool output and source errors. Verify no marker reaches the default ZIP; only the opted-in user and assistant text appears in `report.md`, never in `diagnostics.json`, the GitHub URL or the Discord description. Turn redaction back on and verify the new plan omits the excerpt. Compare both returned preview files byte-for-byte with the archive entries.
- Use a counting provider to verify zero local requests, one model request after Generate, and no retry after a failed or duplicate call. Verify cancellation, expiry, close and reconnect settle work, discard late output and allow local generation from the same evidence while the draft remains open. A late cancellation for a settled plan leaves its successor running.
- Generate a report on a daemon and confirm its `Dim` line carries the same label the binary prints, and that a client whose `clientVersion` names another build adds one `Client` line, in the body and in `evidence.process.clientVersion`. `crates/dim-report/tests/report_flow.rs` covers this at RPC level; `crates/dimcode/tests/cli_flow/report.rs` covers the product daemon and binary.
- Save a report ZIP on a client connected to another host and exercise the GitHub draft and Discord flows without `gh`. Verify explicit sharing follows review or local save and that a failed share leaves the saved bytes available.

## Ablation

The Session namespace serves current-Session reports through the existing prepare method. The process entry is needed for a conversation-open error that fails Session admission and uses the same flow with fewer evidence sources. The plan ID ties a paid request to the exact preview the user confirmed. The bounded ZIP in the generation reply lets the client save without a second download resource or archive store. An isolated one-shot model operation supplies drafting and redaction without a general-purpose report Session.

## Binding appendix

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorReference>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareReply {
    pub report_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<ModelId>,
    pub plan: Plan,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanReply {
    pub plan_id: String,
    pub generation: Generation,
    pub evidence: Reviewed,
    pub instructions_status: String,
    /// The exact prompt material the user reviews before Generate; bounded to 8 KiB.
    pub material: String,
    pub max_output_tokens: u32,
    /// `{amount, currency}` when the owner knows a price, else none.
    pub estimated_cost: Option<Cost>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cost { pub amount: String, pub currency: String }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRequest {
    pub report_id: String,
    pub generation: Generation,
    pub redact: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateReply {
    pub title: String,
    pub body: String,
    pub files: std::collections::BTreeMap<String, String>,
    pub archive: Archive,
    pub sharing: Sharing,
}

#[derive(Debug, serde::Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ReportError {
    #[error("Report methods require a client connection")]
    CallerRequired,
    #[error("{message}")]
    InvalidParams { message: String },
    #[error("The report cannot be prepared now")]
    Unavailable,
    #[error("The report draft is missing or expired")]
    ReportNotFound,
    #[error("The plan was replaced or consumed")]
    ReportPlanStale,
    #[error("A generation is running for this report")]
    ReportBusy,
    #[error("The selected model is unavailable")]
    ReportModelUnavailable,
    #[error("The generation failed")]
    ReportGenerationFailed,
    #[error("The generation was cancelled")]
    ReportCancelled,
}
```

The two namespaces register the same four operations: `session.report.*` on the Session method table and `report.*` on the process table.
