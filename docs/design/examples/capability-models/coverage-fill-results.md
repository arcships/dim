# Coverage-fill TLC run record

Code correspondence: `8a5fe0980`. Java 17.0.18 / TLA+ tools 1.7.4 (TLC 2.19), one worker, 2 GiB heap. All fourteen changed/new/dependent configurations below completed with exit 0 and exhausted queues. [Exact hashes, seeds, properties and evidence reuse](coverage-fill-results.json).

These runs check the [explicit additional scenarios and bounds](coverage-fill.md), not all business promises or Rust implementation refinement. Thirty-two unchanged original configurations retain their exact-hash evidence: thirty passes and the two existing Shell recovery counterexamples. Together these account for the 46 configurations in this first-fill record; this is evidence accounting, not a coverage percentage.

| Configuration | Distinct states | Depth | Exit |
| --- | ---: | ---: | ---: |
| [WaitRuntime](WaitRuntime.cfg) | 2,665,908 | 25 | 0 |
| [WaitSelectionCoverage](WaitSelectionCoverage.cfg) | 2,981,180 | 25 | 0 |
| [WaitHandoff](WaitHandoff.cfg) | 604 | 6 | 0 |
| [RetryChain](RetryChain.cfg) | 44,907 | 22 | 0 |
| [NestedSettlement](NestedSettlement.cfg) | 37,624 | 14 | 0 |
| [HookFollowups](HookFollowups.cfg) | 568 | 25 | 0 |
| [PromptOutcomes](PromptOutcomes.cfg) | 156,808 | 10 | 0 |
| [ToolAdoption](ToolAdoption.cfg) | 3,217 | 18 | 0 |
| [FinalRequest](FinalRequest.cfg) | 176 | 9 | 0 |
| [GoalProgress](GoalProgress.cfg) | 366,024 | 30 | 0 |
| [SnapshotRefresh](SnapshotRefresh.cfg) | 1,778 | 23 | 0 |
| [SnapshotRefresh-Clients](SnapshotRefresh-Clients.cfg) | 1,452 | 23 | 0 |
| [PermissionFacts](PermissionFacts.cfg) | 46 | 6 | 0 |
| [FileBindingQueries](FileBindingQueries.cfg) | 11,110 | 7 | 0 |

The unchanged Wait observation formula `FirstPendingNotice` now passes against the expanded queue model. Its old counterexample remains at `5a78b2ed2`, with its old dependency hash. Do not relabel that old trace as a runtime defect or overwrite it as a passing run.

A separate complete state dump for HookFollowups contains both chains reaching posted rounds `[1,2,3]`; thus the second-chain reset property is exercised, rather than passing because a second chain is unreachable. The actual reachable state and reproduction command are in JSON. This is a positive reachability check, not a deliberately failing invariant.

Development drafts had parser/type errors that were corrected before these final runs. A larger Goal draft was stopped and its redundant full decision-history copy removed; the final transition checks preserve the relevant conditions and completed. No incomplete or timed-out draft is counted as passing evidence. No symmetry or state-constraint filters were used.

A short constructive route to both full chains is: from Init, repeat `End("stop", "success") → Finish(TRUE) → ConsumeFollowup` three times; then `End("stop", "success") → Finish(FALSE) → NormalTurn`; repeat the three-step sequence three more times. Each chain has posted rounds 1, 2 and 3. This route explains reachability; the separately recorded dump state and reproduction command remain the execution evidence.
