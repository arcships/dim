# Cold first `session.attach`

Status: Historical measurement; current restore contract is [Native Session restore read boundary](restore-validation.md). Original profile ([#713](https://github.com/arcships/dim/issues/713)).
Scope: the first `session.attach` after daemon start for a stored, unloaded Session: what is measured,
the release-build cost split, and whether the restore before the reply needs a bound. It changes no
event, service, capability, protocol, configuration or storage format.

Related: [#601](https://github.com/arcships/dim/issues/601) (restore validation reuse), [#588](https://github.com/arcships/dim/issues/588)
(storage and IO audit), [#193](https://github.com/arcships/dim/issues/193) (restore profile),
[#712](https://github.com/arcships/dim/issues/712) (latest-turn window), [idle unload](idle-unload.md),
[restore-scoped shared history read](restore-shared-read.md), [sessions service §6](sessions-service.md#6-methods).

## 1. Summary

`session.attach` acquires a `SessionUse` before it dispatches. When the Session is not loaded,
`Agents::acquire` restores it before `attach` answers. That restore is the first large latency a
Desktop thread open pays; history paging starts only after the reply.

- **Measured.** In a release build over a disposable native store, the cold attach of a Session (its
  `main` branch) is 20.2 ms at 1,001 events, 76.7 ms at 5,001 events and 165.0 ms at 10,001 events
  (plain profile, median of five fresh Agents generations). The product assembly without MCP servers
  or skills is 82.7 ms at 5,005 events and 168.3 ms at 10,017 events. Warm attach is 0.13–0.22 ms. A
  separate instrumented `restore_cost` run at the same build puts a 5k product first restore at
  97.6 ms: validation about 45%, the branch opening's first history read about 39%, the
  `branch/resume` append about 5%, and Session/branch assembly and scheduling about 7%. The attach
  total and that first-restore total are different scopes, not one series (§3.3).
- **Current boundary.** Restore reads Session and branch directory metadata. Each owner reads the objects it needs through the current indexed APIs. There is no first-open full-history validation pass, validation cache, shared decoded opening copy or validation-to-runtime handover. Selected damaged content fails its actual read; unrelated damaged history does not block publication.
- **Measurement scope.** The figures below describe the earlier validation/shared-opening implementation. They are baseline evidence, not timings or requirements for the current path. Client windows, leases and the attach reply keep their own contracts.

## 2. What a cold first attach is

| Term | Meaning in this design |
| --- | --- |
| Cold first attach | The first `session.attach` for a Session id in a new App Server generation (daemon start or an Agents reload) with a native store, while the Session is not loaded |
| Re-acquisition | A later attach in the same generation after idle unload; it restores owners again without a whole-history audit |
| Warm attach | A second attach in the same generation while the Session is loaded and leased; no restore |

The first request that names a Session gets a lease through `Agents::acquire`
([agents.rs:758](../../../crates/dim-app-server/src/agents.rs#L758)); `acquire` authorizes the
project, restores an unloaded Session ([agents.rs:795](../../../crates/dim-app-server/src/agents.rs#L795))
and announces it to Collaboration. `Connection::handle` performs that acquisition before it dispatches
the call ([connection.rs:632](../../../crates/dim-app-server/src/connection.rs#L632)); the attach
handler then builds the reply and installs the subscription
([subscription.rs:405](../../../crates/dim-app-server/src/connection/subscription.rs#L405)).

```text
client                Connection::handle          Agents::acquire        Agent restore                         attach
  | session.attach --------->|                          |                     |                                   |
  |                          | acquire(session) ------->|                     |                                   |
  |                          |                          | restore_session --->| branch directory metadata             |
  |                          |                          |                     | Session and branch assembly       |
  |                          |                          |                     | opening reads, branch/resume      |
  |                          |                          |<--------------------|                                   |
  |                          | attach: Session record, title metadata, listener, approval replay, reply ---------->|
  |<------- reply -----------|                          |                     |                                   |
```

The current restore read boundary is owned by [restore-validation](restore-validation.md). The branch opens, its owners read their required state, and the opening appends `branch/resume` before publication. The historical measurements below separate the old restore spans from the attach call's own work.

Excluded from "cold first attach": the client's connection and `hello`, daemon process startup, the
Desktop fold, and the history paging that #712 owns. They are outside the timed call.

## 3. Historical measurement

### 3.1 Scenario

The measurement uses a disposable native store, never the product home. One Session per size is grown
on `main` through real mock-model turns; no child branch, MCP server, skill or plugin is installed.
Sizes are the committed event count of `main`; the reported count is the head at the first cold attach.

| Item | Setting |
| --- | --- |
| Build | `cargo build --release --locked`, `debug-assertions = false` |
| Machine | AMD Ryzen 9 7945HX, 32 threads, 62 GB RAM, Linux 7.0.0, NVMe |
| Store | SQLite native format, fresh directory per run, `main` only |
| Profiles | `plain`: `defaults::install`, `defaults::mock_model_hosts`, `MockTools`; `product`: the product context window, prepare model and summary strategy shared by both examples |
| Cold sample | A new `serve` generation on the same database; new store and Agents generation, Session unloaded; the Unix socket client is already handshaked; the timer starts when the `session.attach` frame is written and ends when its reply is read |
| Warm sample | The second `session.attach` on the same connection and Session, immediately after the cold one |
| Repetitions | Five per size, interleaved; the JSON reports the minimum, median and maximum, and §3.3 shows the median and maximum |

The process is reused across generations, so the OS page cache and the allocator are warm; a real
daemon start adds its own startup outside the timed call. `attach_cost` covers plain and product
assembly; `restore_cost` covers the instrumented first-restore split; neither installs MCP servers or skills,
and neither depends on the Desktop client.

### 3.2 Commands

The instrumented first-restore split is measured by `restore_cost`:

```text
cargo run --release --locked -p dim-app-server --example restore_cost -- \
    product 1001,5001,6865,10001 5
cargo run --release --locked -p dim-app-server --example restore_cost -- \
    plain 1001,5001,10001 5
```

The cold attach total is measured by `attach_cost` (this PR):

```text
cargo run --release --locked -p dim-app-server --example attach_cost -- \
    plain 1001,5001,6865,10001 5
cargo run --release --locked -p dim-app-server --example attach_cost -- \
    product 1001,5001,6865,10001 5
```

The second argument is the event count per Session: each Session is grown on `main` through
`session.create`, `session.submit` and `session.idle` until its head reaches that count, so the actual
count reported is the head at the first cold attach. The third argument is the repetition count
(default 7). `plain` serves `defaults::install` with `defaults::mock_model_hosts` and `MockTools`;
`product` serves the product context window, prepare model and summary strategy over the same mock
model. Neither contacts a provider.

`ATTACH_COST_ROOT` selects the parent of the disposable home. The default is `/tmp`, and the example
creates `dim-attach-<uuid>` below it, sets mode 0700, and removes it at the end; a root that would make
the socket path longer than 80 bytes fails before `serve`. The example refuses a root inside
`~/.dimcode` before creating anything. The example's `session.submit` carries `branchId: "main"` for
protocol v3.

### 3.3 Cold attach

App Server code at `103d1478` plus the `attach_cost` example in this PR, release build, disposable
store, head before the first cold attach:

| Profile | Events | Cold attach median | Cold attach max | Warm attach median |
| --- | ---: | ---: | ---: | ---: |
| plain | 1,001 | 20.2 ms | 22.6 ms | 0.13 ms |
| plain | 5,001 | 76.7 ms | 82.2 ms | 0.20 ms |
| plain | 6,871 | 112.7 ms | 119.5 ms | 0.18 ms |
| plain | 10,001 | 165.0 ms | 175.8 ms | 0.22 ms |
| product | 5,005 | 82.7 ms | 90.8 ms | 0.14 ms |
| product | 10,017 | 168.3 ms | 210.9 ms | 0.22 ms |

Warm attach is below a quarter of a millisecond at every size. The attach table and the restore table
are not one series. `attach_cost` times only `session.attach`; `restore_cost` times `restore_session`
plus the first `idle("main")` under a counting global allocator and a forwarding `TimelineStore`
(`restore_cost.rs`). Both include the restore, but the instrumented restore is measured on a different
boundary with different overhead, so a smaller attach row is possible and the two totals cannot be
subtracted. The attach call itself does not add a history-dependent step. The product rows install the
product capabilities but no MCP server or skill; that startup is the follow-up in §6. Every cold attach
appends one `branch/resume`, so later repetitions time a store one event larger per prior repetition;
the table uses the head before the first cold attach.

### 3.4 Instrumented first restore

`restore_cost` first restore of each Session at the same App Server code (`103d1478`) plus this PR's
examples, release build, five repetitions per size. `Validation` is the store's `branches` pass;
`Opening reads` is the union of the store reads made while the branch opens; `branch/resume` is the
append until it is published; `Outside store` is the rest (Session and branch assembly, capability
rebuild, scheduling). Spans can overlap; the rows are medians, so their sum is not exactly the total.
This table reports the restore's internal share only; it is not the attach's decomposition.

| Mix | Events | First restore | Validation | Opening reads | `branch/resume` | Outside store |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| product | 1,014 | 27.0 ms | 9.5 ms | 7.6 ms | 4.8 ms | 2.2 ms |
| product | 5,010 | 97.6 ms | 45.3 ms | 38.2 ms | 4.5 ms | 7.1 ms |
| product | 6,873 | 128.5 ms | 55.6 ms | 54.1 ms | 3.6 ms | 7.8 ms |
| plain | 1,006 | 25.2 ms | 8.1 ms | 6.7 ms | 4.7 ms | 2.6 ms |
| plain | 5,006 | 86.5 ms | 37.8 ms | 32.4 ms | 4.9 ms | 8.0 ms |

At 5k and 6.9k, validation and the opening read are about 85% of the first restore (validation about
43–46%, the opening read about 39–42%); the append is 3–5% and the outside-store work 6–7%. The 10k
first-restore split was not re-run at this build; only the 10k attach total in §3.3 is reported.

The product 5k row is 11.1 ms above plain (97.6 against 86.5). The columns put that difference in
validation (+7.5 ms) and the opening read (+5.8 ms), not in outside-store rebuild (−0.9 ms). Product
growth writes larger event bodies (context/prepare and summaries), so product and plain at an equal
event count are not equal work; the product delta is per-event decode cost, not the capabilities'
rebuild.

### 3.5 What the tables do and do not cover

Covered: the App Server's attach path, the native store, full validation on first use, the branch
opening and its shared read, `branch/resume`, Session and branch assembly, and the attach reply and
subscription.

Not covered: project authorization before the restore, MCP server startup, skill/TypeScript binding,
a real model call, and a Desktop client. The issue's table is a debug build (155–925 ms) while §3.3 is
release; the live daemon's 0.75–1.9 s is not reproduced by a release build at these sizes. The
difference includes the build profile, one of the uncovered steps, or a different event mix/machine.
The next measurement isolates the product startup rather than guessing it (§6).

## 4. Historical cost breakdown

- **Two decodes in the measured old path.** Validation decoded every stored event and the shared opening decoded them again. Both mechanisms have since been removed; this is not the current first-open requirement.
- **Sequential, one connection.** Validation finishes before the branch is built, and the store has
  one connection; the two decodes cannot overlap. Both are linear in the event count, so the cold
  attach is linear: 20.2 → 76.7 → 165.0 ms for 1k → 5k → 10k. Nothing is superlinear after #235/#239.
- **Smaller steps.** The `branch/resume` append pays one commit and is roughly constant. Session and
  branch assembly, capability rebuild and scheduling are about 7% at these sizes. The attach call's
  own work is a metadata read for the title, a listener registration, pending-approval replay and the
  reply; it does not read history.
- **Warm is different.** Once loaded and leased, attach installs a listener and replies; the measured
  warm median is 0.13–0.22 ms, so the cost is entirely in the cold restore.

## 5. Current decision and historical budget

The current decision is directory admission plus validation at actual reads, as specified in [restore-validation](restore-validation.md). The old requirement to validate the entire history on first open and the validation handover proposal are withdrawn. This page does not add an admission scan, repair pass or cache.

The following provisional budgets and measurements belong to the old implementation. They do not establish current performance or acceptance:

| Size | Profile | Provisional cold-attach budget | Measured |
| ---: | --- | ---: | ---: |
| 5k | plain | 100 ms | 76.7 ms |
| 5k | product, no MCP/skill | 150 ms | 82.7 ms |
| 10k | plain | 200 ms | 165.0 ms |
| 10k | product, no MCP/skill | 300 ms | 168.3 ms |
| any | warm | 5 ms | 0.13–0.22 ms |

The product rows are `attach_cost product` without MCP servers or skills; the follow-up adds those
and closes the remaining gap to the issue's live-daemon numbers. The product budgets deliberately
leave room for that startup (about +80% over the measured product rows), while the plain budgets are
about +20–30% over their rows. The budget is a target for a later change, not an acceptance that
blocks this design.

No Desktop preload or cache is required: an attached client is a lease holder, and pinning Sessions
   would conflict with idle unload. A preload would move the same work before the user asks for it.

A lazy attach or a catalog-derived restore was considered and rejected for now: it would shift the
wait to the first history read rather than remove it, and it would change the reply's `sequence`, the
live subscription, pending-approval replay and the lease that keeps the Session loaded. That is a
separate protocol and lifecycle design, not a profile of the restore.

## 6. Historical follow-up: Evidence that would justify a later change

The remaining unknown is the product startup that neither `restore_cost` nor the plain `attach_cost`
includes. The follow-up extends `attach_cost` (or adds an example) to install one MCP server and one
skill; the current example only takes `plain|product`, sizes and repetitions. Measure it before
changing the restore path:

| Observation | Conclusion |
| --- | --- |
| The extended product `attach_cost` at 5k and 10k stays within the §5 budget and the two store spans remain about 85% of the instrumented first restore | Close #713 as measured; no change |
| The product total exceeds the budget and MCP/skill startup is more than half of it | The change belongs to MCP/skills startup, not to attach or the restore path; open it with that owner |
| The product total exceeds the budget and current owner reads remain the largest step after startup is separated | Measure those actual reads under the current restore boundary |
| The issue's live-daemon 0.75–1.9 s is reproduced | Profile that environment; it is outside the release baseline measured here |

The attach call's own span (Session record, title metadata, listener, approval replay, reply) is not
isolated by either example; deriving it by subtracting the restore table is invalid across the two
scopes (§3.3), so it is not a usable trigger until the harness reports it.

A cold attach that is already inside the budget at 1k, 5k and 10k does not justify a read-path change
whose only effect is to remove one decode.

## 7. Withdrawn candidate: reuse validation output

This candidate is withdrawn. There is no admission validation pass or decoded opening copy to reuse.
Cold and warm restore follow the [restore read boundary](restore-validation.md). The profiling scenarios and
historical measurements above remain evidence about the versions measured, not requirements for a startup scan.

## 8. Requirements raised to mechanism owners

No mechanism requirement remains from this historical profile. The validation-output handover was withdrawn; current requirements belong to [restore-validation](restore-validation.md) and the owners of the selected reads.

## 9. Rule check

This is an App Server measurement and a decision about its attach path, not a plugin, capability or
event design. The applicable rules:

| Rule | Answer |
| --- | --- |
| [Principles §4](../../architecture/rust-agent/principles.md#4-通过时间线扩展行为) rule 1 (command events) | Not applicable: the measurement changes no state and writes no event |
| §4 rule 2 (handover) | Not applicable: nothing is handed over |
| §4 rule 3 (`before`) | Not applicable: no producer or draft changes |
| §4 rule 4 (no business meaning in core) | No core mechanism learns about attach; the measurement reads the store's existing spans |
| §4 rule 5 (atomic batches) | The measured reads and the candidate keep the existing batch-boundary rules |
| §4 rule 6 (model context) | Unchanged: no context input or insertion path is added |
| §4 rule 7 (public events and services) | No service or event changes; attach and the Session facade keep their contracts |
| §4 rule 8 (infrastructure) | No Timeline, Dispatcher, event envelope, assembly or facade change. The candidate would raise at most one sentence to the store's read contract |
| §4 rule 9 (facts in the Timeline) | No new fact; the measured work is a read |
| §1 (Capability and App Server boundary) | The restore and attach are the App Server's own path; no Capability is added |
| [ADR 002](../../adr/002-plugin-scope.md) D1 (scope follows state) | Process scope: the measurement runs in the App Server process; no new state |
| ADR 002 D2 (no wiring modules) | No new module or plugin |
| ADR 002 D3 (dependencies follow scope) | No new dependency |
| ADR 002 D4 (configuration) | None |
| ADR 002 D5 (language-neutral contracts) | No public contract changes; the candidate is internal |

## 10. Documentation changes

With this design:

- [the documentation index](../../INDEX.md) links this page.
- [restore-scoped shared history read](restore-shared-read.md) and the #601 design already assign the
  current read boundary to [restore-validation](restore-validation.md); this page retains historical attach measurements.
- [idle unload](idle-unload.md) and [session history window](session-history-window.md) already state
  that an attach may restore the Session; they do not change.

With the measurement follow-up:

- Extend `attach_cost` (or add an example) to install one MCP server and one skill, and add the
  product rows at 1k, 5k and 10k.
- Any future read optimization must use the current selected-read contract; the withdrawn validation handover is not an implementation option.

## 11. Acceptance and next steps

**This design.** The measurement scenario is defined. The attach total is measured at 1k, 5k, 6.9k
and 10k plain and at 5k/10k product without MCP/skill; the instrumented first-restore split is
measured at 1k, 5k and 6.9k on the same App Server code. `attach_cost` is in this PR and reproduces
§3.3. The decision is to keep the first attach as a restore before the reply and to set the
provisional budget of §5.

**Follow-up measurement.** Extend `attach_cost` (or add an example) to install one MCP server and one
skill, run it for the product profile at 1k, 5k and 10k, five repetitions, and report the attach
total, the instrumented first-restore split, and the plugin-startup span. The attach-only span is not
isolated by the current harness; add it before using it as a trigger. Then:

- within the budget at all sizes: close #713 with this design and the numbers;
- over the budget for a startup step: open the change with that owner;
- over the budget for storage: profile current owner queries and propose a bounded read change with before/after measurements.

No Desktop preload, cache or pinned Session is part of any step.
