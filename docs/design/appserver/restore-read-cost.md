# Cold restore read and computation cost

Scope: measurement of native directory reads, owner restoration and client consumption. Related: [#1320](https://github.com/arcships/dim/issues/1320), [restore read boundary](restore-validation.md), [cold attach](cold-attach-profile.md).

## 1. Work to measure

| Phase | Required result | Owner |
| --- | --- | --- |
| Native directory read | Parsed Session configuration and usable branch metadata | Native store; indexed directory reads, no history preflight |
| Business restoration | Current state and unfinished owned work | Each business owner |
| Assembly and opening | Required parts, resources and publication receipts | The parts that acquire or start them |
| History and usage | Requested display window and current statistics | Their own interfaces |

There is no restore integrity-scan phase and no validation cache. Event and content decoding occurs only for objects an actual consumer reads. Format upgrades are measured separately from restoring an already upgraded database.

## 2. Measurement

Use disposable homes and identical logical histories. Compare new-process restoration of an already upgraded session with the first upgrade of an old session. Record database open/upgrade, Session restore, first usage, repeated usage and first display separately. CPU time and wall time are different measurements.

Record native SQL statement/VM/scan/sort counts separately from timings so plan diagnostics do not contaminate CPU measurements. Rust live/peak allocations exclude SQLite native allocations; report process RSS as a separate sample. Do not add or subtract phases from different runs.

Legacy Usage restoration must reuse compatible saved contributions: no historical message recount and no historical cumulative-result backfill. A completely absent statistic or a cold uncovered source leaves statistics unavailable until a real request supplies its final projection. No cold replay or old-cut recount occurs. See [ContextUsage](../plugins/ContextUsage.md).

## 3. Acceptance

On the same stored long-session fixture, directory restoration must read no historical event body for admission. Required object reads still fail explicitly when their selected content is damaged. Test directory changes and process restart without a validation cache, and keep transaction rollback and supported-format tests.

Report measured before/after values and their build profile. Indexed subquery success does not prove end-to-end first-frame performance. Assembly, external resource startup and necessary request construction remain separately accountable.

## 4. Cold product acceptance

Use a new daemon and an actual CLI PTY, measuring launch through visible history and editable input. Before each sample, sync and evict the disposable database, executable and configuration files and verify zero residency for those files. Do not prewarm a daemon or retain those file pages as an acceptance condition. The kernel may populate file cache during execution; report that separately from anonymous memory.

Apply a cgroup memory limit with swap disabled to the CLI, daemon and probe together; report the full group peak, anonymous/file memory, read-call bytes, physical read bytes, process CPU and OOM/limit events. Probe memory is reported, not casually subtracted. Run identical fixtures before and after, with compilers stopped. SQL/read tracing is a separate diagnostic run because tracing alters latency.

Normal tail reads select bounded published positions, not whole turns or atomic batches. Completed compactions project summary fields once without loading replacement into Rust. Metadata still requires the inline JSON pages in the existing format. Explicit detail, raw export and first format upgrade are separate workloads.

## 5. Remaining selected-object work

Branch terminal lookup uses the existing text candidate for a generated UUID, followed by the exact kind and branch identity condition. The range is the UUID's indexed matches, which can include other facts mentioning that UUID; it is not proven constant by `LIMIT 1`. Arbitrary identities written by a trusted `before` handler that cannot form a safe trigram candidate retain the existing kind-range fallback. No new branch-state snapshot or native format is introduced in this change.

An unavailable Usage number does not block restoration. Its first actual request can perform the existing Loop projection plus Policy's first contribution projection; subsequent initialized requests reuse the number. Count these two first-request projections separately from zero cold-history projections. Prompt's public state publication still copies its public snapshot; only repeated temporary copies were removed. Existing three-format tool rendering remains necessary for terminal blocks whose source row has left the bounded window.

## 6. Earlier v43 measurement on the long-session fixture

Source `cd6bbce4fcaf8079e2faf52bd1a7b40192841a5a`, native v43, release binary SHA-256 `dcac7e5a1b14ff8624eec16b85264560c52ac38239d00d199f890f9ff8b46e1e`. The fixture has about 25,049 physical event rows and 1,000 mock turns. Each cell below contains two independent cold CLI PTY samples; the 256 MiB limit includes the probe, CLI, daemon and new file cache, with swap disabled. Files were verified nonresident before every sample. Baseline source is `6164eab570f9c896b673294798258d29db8b517d`.

| Fixture | Baseline editable ms | Changed editable ms | Changed daemon CPU ms | Changed group peak MiB | Changed daemon read-call MB |
| --- | --- | --- | --- | --- | --- |
| Saved statistics | 215.6 / 219.7 | 248.0 / 234.6 | 310 / 270 | 154.0 / 152.9 | 4.3 / 4.1 |
| Completed external work | 195.9 / 202.4 | 198.2 / 204.0 | 220 / 220 | 153.8 / 153.8 | 3.9 / 3.8 |
| 2.43 MB compaction, no statistics | 228.6 / 221.8 | 211.8 / 213.8 | 230 / 240 | 165.0 / 164.9 | 8.5 / 8.5 |

At this earlier revision the 200 ms end-to-end target was not met and the ordinary fixture regressed. Sections 7–8 record the subsequent measurements. Initial display selection changed from the latest turn to a fixed tail, so these are user-flow comparisons, not isolated query speedups. At 192 MiB, editable times were 247.5–258.5, 196.7–205.0 and 212.7–218.9 ms respectively, without limit or OOM events.

The compaction's 594 overflow pages fell from seven reads (17,031,168 bytes) to two reads (4,866,048 bytes) in one summary SELECT. A separate VFS diagnostic verified this; its timings are excluded. Forcing a materialized raw JSON or JSONB intermediate increased that query's measured CPU median from 2.24 ms to 5.32/6.03 ms and copied the whole body, so it was rejected. A supplementary mixed-kind position query also increased body reads and was removed. Position-only projections still reuse selected coordinates.

Read-call bytes are not physical disk bytes: the changed compaction sample still physically read about 25 MB in the daemon and 35.5 MB in the CLI. The full group held about 33–35 MiB anonymous memory and 114–122 MiB file pages; daemon anonymous memory was about 15–17 MiB. The earlier compaction group peak was about 175 MiB. Do not describe the entire peak reduction as retained heap savings.

A separate direct-protocol diagnostic, without a CLI or UI registration, measured listener readiness at 49–50 ms, hello at 17–24 ms, attach at 79–87 ms and display at 37–44 ms. These stages include current-owner assembly, necessary selected reads and opening writes. Display processes at most eight batches of eight positions; small-batch query round trips also cost time. The CLI additionally registers its UI, waits for catalog/permission/skills responses and renders. The diagnostic does not isolate a single cause for the ordinary user-flow regression and cannot be added to stages from PTY runs.

One ordinary opening produced 13 existing facts (5,095 JSON bytes): one resume, four date facts, six client-presence input/application facts and two execution-setting facts. There were no new recovery snapshots or Usage numeric backfills. These writes still maintain indexes and WAL.

First upgrade is separate: one cold product sample from v41, Timeline-v42 and main-v42 took 320.3, 312.6 and 322.9 ms under 256 MiB, peaking near 206 MiB. Under 192 MiB they took 309.3, 317.9 and 312.1 ms, reached the limit and triggered reclaim (64–90 max events), with no OOM or swap. Upgrade includes missing relationship construction and main's before/after foreign-key checks; ordinary v43 opening performs neither historical pass. Two samples per normal fixture and one per upgrade source are evidence about this machine and workload, not a universal latency guarantee.


## 7. Indexed display measurement, native v44

Measured implementation: `396ddcd95`, compared with `a524b9668` on the same three disposable fixtures. Changed release binary SHA-256: `7da2b5029a51bfc0da6c7693ec74a0d3f00f498400eae9e787f1c28a22f4db99`; baseline: `42964815a191908237f56fc5081c5029ae1fad36a2058055f8c3d828508bde80`. Subsequent documentation edits do not change either binary. Both use native v44; this display change adds no index, format upgrade, business event or persistent cache.

The ordinary fixture has 25,049 physical event rows, 1,000 mock turns and 10,667,080 event-body bytes. The completed-work fixture has 25,954 rows and 10,943,264 body bytes. The compaction fixture has 25,050 rows and 13,127,097 body bytes, including the 2.43 MB replacement. These are test fixtures, not a production-session sample.

### Cold launch through editable history

Each cell contains two independent actual CLI PTY runs. Before every run the disposable database, executable and configuration files were synced, evicted and verified nonresident. Each run starts a new daemon; system libraries retain their natural cache state. The 256 MiB/no-swap cgroup includes the probe, CLI, daemon and newly populated file cache. No compiler runs during measurement. All samples had zero limit/OOM events. Format preparation occurs outside the normal-load timer; this round does not remeasure first upgrade.

| Fixture | Before editable ms | After editable ms | Before daemon CPU ms | After daemon CPU ms | Before group peak MiB | After group peak MiB |
| --- | --- | --- | --- | --- | --- | --- |
| Saved statistics | 233.7 / 233.8 | 163.7 / 163.8 | 270 / 280 | 160 / 160 | 157.6 / 153.5 | 154.7 / 154.0 |
| Completed external work | 210.2 / 199.2 | 160.8 / 157.9 | 240 / 230 | 170 / 160 | 153.9 / 154.3 | 155.4 / 155.5 |
| Compaction, no statistics | 196.7 / 201.2 | 167.0 / 165.9 | 210 / 220 | 160 / 160 | 164.4 / 164.2 | 166.8 / 170.8 |

All six changed samples meet 200 ms on this machine and these fixtures. Two samples per fixture are not a latency percentile or a guarantee for arbitrary sessions. Compared by sample means, elapsed time falls about 30%, 22% and 16%; daemon CPU falls about 42%, 30% and 26%. This combines indexed display selection and removing catalog/permission/skills replies from the CLI's initial-history wait; it does not isolate their individual timing contributions. The latter replies still arrive through the existing worker and are applied after display is available.

Memory does not show a general reduction. At editable history, changed full-group anonymous memory is 32.5–34.3 MiB versus 33.2–34.9 MiB before. Changed file memory is 116.9–128.1 MiB versus 115.1–121.4 MiB. The compaction group's peak increases about 2.4–6.6 MiB. These editable-time categories and the group peak are different samples and must not be subtracted to invent a peak allocation breakdown. File pages were populated during the measured run, not retained as a precondition.

| Fixture | Before daemon read-call MiB | After daemon read-call MiB | Before daemon physical-read MiB | After daemon physical-read MiB |
| --- | --- | --- | --- | --- |
| Saved statistics | 3.99 / 4.02 | 4.68 / 4.72 | 23.13 / 22.91 | 24.16 / 23.91 |
| Completed external work | 3.89 / 3.70 | 4.01 / 4.06 | 23.13 / 23.51 | 24.51 / 24.60 |
| Compaction, no statistics | 7.92 / 7.88 | 8.38 / 8.40 | 23.90 / 23.79 | 24.76 / 25.71 |

Read-call and physical-read bytes increase in this comparison. Kind-based pages contain more selected display events than equal-sized raw-position windows, so this is not an equal-payload I/O comparison. The confirmed benefit is latency and CPU, not lower disk traffic or lower total memory. Remaining work includes actual selected bodies, inline compaction JSON pages, owner opening and its existing writes, executable/library pages, transport and rendering. This run does not independently time each remaining source. Post-teardown event totals include close events and are not startup-write counts.

### Query work and pagination

The selector uses the existing kind index. Ordinary events supply sequence coordinates; packed text supplies only its sequence bounds, from which the requested logical coordinates are produced. Each kind contributes at most the page limit plus one coordinates; only the final selected page loads bodies. No candidate-body JSON expansion or whole-history filtering is required for this query shape.

The existing public storage flow compares native and memory results, clipped packed boundaries, cursor direction and the default ordering. Across two measured kind-position pages, histories with 100 and 1,000 source items both execute 247 SQLite VM instructions in the recorded selection statements, with zero full-scan steps, SQL sorts or automatic indexes. Branch-head validation remains enabled and is outside these selection counters. This is evidence for this query shape and fixture, not a claim that all Fields queries have constant cost.

An earlier trial (`18b04472d`) used the generic mixed-kind selector, which decoded packed candidates before the final page bound. It reached 200–204 ms on the ordinary fixture and raised group memory to about 167 MiB. Replacing that candidate expansion with coordinate-only indexed selection removed the added work. No cache or new index was needed.

The following direct-protocol diagnostics reuse normal caches and exclude the CLI. Their elapsed times are not cold acceptance results. The earlier-page probe begins at a fixed head minus 128; it is not a click after the first page in the table. CPU resolution is 10 ms. Initial diagnostics can include outstanding opening work. Web rows measure the Web pagination policy through RPC, not browser paint time.

| Fixture / policy | Pages before → after | Returned events before → after | Payload bytes before → after | RPC elapsed ms before → after | Daemon CPU ms before → after |
| --- | --- | --- | --- | --- | --- |
| Ordinary / initial | 2 → 2 | 94 → 128 | 24,737 → 47,629 | 75.1 → 55.5 | 60 → 20 |
| Completed / initial | 1 → 1 | 49 → 64 | 10,677 → 13,070 | 36.1 → 17.6 | 20 → 10 |
| Compaction / initial | 1 → 1 | 57 → 64 | 12,976 → 14,925 | 41.0 → 23.8 | 30 → 10 |
| Ordinary / earlier | 4 → 2 | 327 → 512 | 153,001 → 284,275 | 489.0 → 233.8 | 330 → 10 |
| Completed / earlier | 4 → 4 | 737 → 1,024 | 176,873 → 363,854 | 547.7 → 310.1 | 370 → 30 |
| Compaction / earlier | 4 → 1 | 246 → 256 | 137,227 → 159,008 | 455.7 → 131.6 | 320 → 10 |
| Ordinary / Web initial | 4 → 2 | 343 → 128 | 140,889 → 47,629 | 387.3 → 40.5 | 250 → 10 |
| Completed / Web initial | 4 → 1 | 605 → 64 | 133,576 → 13,070 | 373.9 → 12.8 | 280 → 10 |
| Compaction / Web initial | 4 → 1 | 310 → 64 | 145,311 → 14,925 | 365.3 → 18.4 | 250 → 10 |

The window remains bounded, not guaranteed full: all input/message events remain eligible, and Shell/Hook or other internal input content may not form a chat entry. The existing four-page cap therefore remains reachable, as the completed-work earlier-page probe shows. No prefix index or source classification is introduced. The initial page has 64 selected events and earlier pages 256; these are event counts, not rendered-message counts.

Validation for this increment: 102 relevant Rust flow cases and 17 Web cases passed, including the corrected ACP window fixture and final storage/history reruns; one existing ACP case remains ignored. Affected all-target Clippy, Web type checking, formatting and ADR checks passed. Earlier workspace-wide evidence belongs to the preceding commits; the full workspace suite was not rerun for this increment.


## 8. Size the first selected page independently

Implementation `4939d01fb` changes only the shared Rust client window and the Web window. The first negotiated transcript page requests 32 selected events. If no transcript row is present, later pages request 64; the read still stops at the first visible page, the branch start or four pages. The maximum initial selection is now 224 events rather than 256. Explicit earlier reads retain their own 256-event page, and legacy server fallbacks remain unchanged. No new API, index, cache or query is introduced.

Keeping 64 after changing a page from raw positions to selected kinds was not a calibrated first-page choice. A direct-protocol comparison on the same fixed-head fixtures tested four choices:

| Initial policy | Ordinary pages / events / visible identities | Completed pages / events | Compaction pages / events |
| --- | --- | --- | --- |
| Always 16 | 4 / 64 / 0 | 1 / 16 | 1 / 16 |
| Always 32 | 3 / 96 / 5 | 1 / 32 | 1 / 32 |
| Always 64 | 2 / 128 / 16 | 1 / 64 | 1 / 64 |
| First 32, then 64 | 2 / 96 / 5 | 1 / 32 | 1 / 32 |

Always 16 fails to reach the ordinary fixture's latest chat before the four-page bound. Always 32 requires another round trip. First 32, then 64 reduces the dense-tail read without adding a round trip in these fixtures. This does not promise every arbitrary sparse history will contain a visible row within the cap, or that a fixed event count fills a viewport. Missing earlier content remains explicitly marked and available through the existing earlier-history action.

| Fixture | Selected events before → after | Returned JSON bytes before → after | RPC pages before → after |
| --- | --- | --- | --- |
| Ordinary | 128 → 96 | 47,629 → 29,594 | 2 → 2 |
| Completed work | 64 → 32 | 13,070 → 6,491 | 1 → 1 |
| Compaction | 64 → 32 | 14,925 → 7,395 | 1 → 1 |

The returned body volume falls by 38%, 50% and 50%. This quantity alone cannot explain a multi-MiB whole-process peak change. The compaction's inline JSON still has to be read for its summary, and executable/library file pages are not removed by reducing the display page. Anonymous memory, file cache, physical reads and the group peak must be measured separately rather than inferred from returned bytes.

Ablation: the implementation reuses whether the existing window has received its first page. There is no new counter or adaptive-controller object. The larger fallback page is retained because the fixed-32 comparison adds an RPC; fixed-16 loses the ordinary fixture's visible history. Changed client and ACP flows verify the first/continuation limits, fixed head and live-event handling. The existing earlier-read and old-server flows retain their contracts.


### Cold product check after sizing

Release source `4939d01fb`; SHA-256 `26393c0d9eb208b0e0d8d4be415aef4870a3936add4a0b194a77236a0fc787f8`. The comparison reruns the frozen §7 binary rather than comparing only with yesterday's timings. Each binary has two runs per fixture, using the same §7 256 MiB/no-swap procedure. Executable, database and configuration residency was zero before every run; all twelve runs had zero limit/OOM events. No compiler ran during these measurements. Both are already v44, with no upgrade in the timer.

| Fixture | Before editable ms | After editable ms | Before group peak MiB | After group peak MiB | Before daemon CPU ms | After daemon CPU ms |
| --- | --- | --- | --- | --- | --- | --- |
| Ordinary | 175.4 / 171.1 | 167.0 / 161.4 | 154.5 / 155.3 | 154.7 / 155.4 | 190 / 180 | 180 / 160 |
| Completed work | 165.2 / 158.7 | 156.7 / 161.9 | 156.1 / 154.8 | 153.5 / 154.7 | 160 / 160 | 160 / 170 |
| Compaction | 171.9 / 162.5 | 169.6 / 170.4 | 166.9 / 166.8 | 165.0 / 166.7 | 170 / 150 | 170 / 160 |

All six new runs display the fixture's latest expected history and render a typed key within 200 ms. The ordinary fixture improves in these samples; completed-work and compaction timings/CPU do not establish an additional improvement. Two runs are insufficient to assign significance to small changes.

| Fixture | Before anonymous MiB | After anonymous MiB | Before file MiB | After file MiB | Before daemon read-call MiB | After daemon read-call MiB |
| --- | --- | --- | --- | --- | --- | --- |
| Ordinary | 33.04 / 33.25 | 33.21 / 33.07 | 117.22 / 117.32 | 116.91 / 117.24 | 4.92 / 4.95 | 4.67 / 4.72 |
| Completed work | 32.68 / 32.51 | 32.36 / 32.73 | 118.59 / 117.50 | 116.93 / 117.11 | 4.23 / 4.21 | 3.87 / 4.04 |
| Compaction | 34.32 / 34.28 | 34.31 / 34.33 | 124.21 / 123.52 | 122.47 / 123.42 | 8.34 / 8.26 | 8.36 / 8.29 |

These anonymous/file categories are full-group samples at editable history, not a decomposition of the earlier peak. Anonymous memory is effectively unchanged. Peak memory is flat in the ordinary fixture and modestly lower in the other two; the samples do not establish a large or universal memory saving. Daemon physical reads change from 24.07/24.07 to 23.89/23.96 MiB, 24.48/24.43 to 23.82/23.97 MiB, and 24.68/24.81 to 24.19/24.43 MiB respectively. All file memory still counts toward the group limit.

The correction reduces selected content and avoids unnecessary work even though the selected display payload is only tens of KiB. It does not eliminate the existing program/file-page footprint or compaction summary's inline-body reads. Those costs remain in the measurement, and no causal claim assigns the multi-MiB peak movement solely to display bytes.

Validation for this sizing correction: 3 shared-client flows, 1 ACP load flow, 5 CLI window/attach flows and 17 Web flows passed; affected client/CLI/ACP all-target Clippy, Web type checking, formatting and ADR checks passed. Server code, storage format and indexes are unchanged, so the preceding server-query evidence remains applicable.
