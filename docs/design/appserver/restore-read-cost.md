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

## 6. Release measurement on the long-session fixture

Source `cd6bbce4fcaf8079e2faf52bd1a7b40192841a5a`, native v43, release binary SHA-256 `dcac7e5a1b14ff8624eec16b85264560c52ac38239d00d199f890f9ff8b46e1e`. The fixture has about 25,049 physical event rows and 1,000 mock turns. Each cell below contains two independent cold CLI PTY samples; the 256 MiB limit includes the probe, CLI, daemon and new file cache, with swap disabled. Files were verified nonresident before every sample. Baseline source is `6164eab570f9c896b673294798258d29db8b517d`.

| Fixture | Baseline editable ms | Changed editable ms | Changed daemon CPU ms | Changed group peak MiB | Changed daemon read-call MB |
| --- | --- | --- | --- | --- | --- |
| Saved statistics | 215.6 / 219.7 | 248.0 / 234.6 | 310 / 270 | 154.0 / 152.9 | 4.3 / 4.1 |
| Completed external work | 195.9 / 202.4 | 198.2 / 204.0 | 220 / 220 | 153.8 / 153.8 | 3.9 / 3.8 |
| 2.43 MB compaction, no statistics | 228.6 / 221.8 | 211.8 / 213.8 | 230 / 240 | 165.0 / 164.9 | 8.5 / 8.5 |

The 200 ms end-to-end target is not met. The ordinary fixture regressed. Initial display selection changed from the latest turn to a fixed tail, so these are user-flow comparisons, not isolated query speedups. At 192 MiB, editable times were 247.5–258.5, 196.7–205.0 and 212.7–218.9 ms respectively, without limit or OOM events.

The compaction's 594 overflow pages fell from seven reads (17,031,168 bytes) to two reads (4,866,048 bytes) in one summary SELECT. A separate VFS diagnostic verified this; its timings are excluded. Forcing a materialized raw JSON or JSONB intermediate increased that query's measured CPU median from 2.24 ms to 5.32/6.03 ms and copied the whole body, so it was rejected. A supplementary mixed-kind position query also increased body reads and was removed. Position-only projections still reuse selected coordinates.

Read-call bytes are not physical disk bytes: the changed compaction sample still physically read about 25 MB in the daemon and 35.5 MB in the CLI. The full group held about 33–35 MiB anonymous memory and 114–122 MiB file pages; daemon anonymous memory was about 15–17 MiB. The earlier compaction group peak was about 175 MiB. Do not describe the entire peak reduction as retained heap savings.

A separate direct-protocol diagnostic, without a CLI or UI registration, measured listener readiness at 49–50 ms, hello at 17–24 ms, attach at 79–87 ms and display at 37–44 ms. These stages include current-owner assembly, necessary selected reads and opening writes. Display processes at most eight batches of eight positions; small-batch query round trips also cost time. The CLI additionally registers its UI, waits for catalog/permission/skills responses and renders. The diagnostic does not isolate a single cause for the ordinary user-flow regression and cannot be added to stages from PTY runs.

One ordinary opening produced 13 existing facts (5,095 JSON bytes): one resume, four date facts, six client-presence input/application facts and two execution-setting facts. There were no new recovery snapshots or Usage numeric backfills. These writes still maintain indexes and WAL.

First upgrade is separate: one cold product sample from v41, Timeline-v42 and main-v42 took 320.3, 312.6 and 322.9 ms under 256 MiB, peaking near 206 MiB. Under 192 MiB they took 309.3, 317.9 and 312.1 ms, reached the limit and triggered reclaim (64–90 max events), with no OOM or swap. Upgrade includes missing relationship construction and main's before/after foreign-key checks; ordinary v43 opening performs neither historical pass. Two samples per normal fixture and one per upgrade source are evidence about this machine and workload, not a universal latency guarantee.
