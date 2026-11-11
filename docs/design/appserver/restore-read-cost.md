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

Legacy Usage restoration must reuse compatible saved contributions: no historical message recount and no historical cumulative-result backfill. A completely absent statistic is a distinct initialization case. See [ContextUsage](../plugins/ContextUsage.md).

## 3. Acceptance

On the same stored long-session fixture, directory restoration must read no historical event body for admission. Required object reads still fail explicitly when their selected content is damaged. Test directory changes and process restart without a validation cache, and keep transaction rollback and supported-format tests.

Report measured before/after values and their build profile. Indexed subquery success does not prove end-to-end first-frame performance. Assembly, external resource startup and necessary request construction remain separately accountable.
