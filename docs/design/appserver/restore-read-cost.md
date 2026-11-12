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
