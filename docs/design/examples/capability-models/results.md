# TLC run record

Historical artifacts: the hashes below refer to commit `c885c8eab`. WaitRuntime and NestedSettlement have since been expanded; use [the coverage-fill record](coverage-fill-results.json) for their current results. Unchanged artifacts retain the evidence below.

Scope clarification: this is the original 34-configuration experiment, not a complete per-Capability coverage verdict. See the subsequent [coverage audit](coverage-audit.md) and its separate Wait selection probe; none of these recorded models/configurations was changed by that audit.

Date: 2026-11-15. Code correspondence: `8a5fe0980`. Java 17.0.18, TLA+ tools release 1.7.4, TLC 2.19 (rev 5a47802), one worker, 2 GiB maximum heap.

**34 configurations executed: 32 passed; two ShellSession recovery configurations produced temporal counterexamples.** All searches exhausted their state queues. A counterexample is a failed property, not a passing test. There were no parser errors or timeouts in this final run.

[Obligations, assumptions and commands](README.md) | [Machine-readable records and SHA-256 hashes](results.json) | [Actual counterexample traces](counterexamples.md)

The runs explore 1,234,293 distinct states in total across separate models; these are not states of one composed system. The 177 configured invariant/property entries include repeated properties across configurations and must not be counted as independent business promises. Bounds are the explicit finite domains and guards in each module and the constants in its configuration. No constraint filters or symmetry reductions were used.

| Configuration | Outcome / exit | Distinct states | Depth | Model SHA-256 prefix |
| --- | --- | ---: | ---: | --- |
| [AdmissionAdvice](AdmissionAdvice.cfg) | PASS / 0 | 2,442 | 7 | `8174450b8eab` |
| [BranchLine](BranchLine.cfg) | PASS / 0 | 308,340 | 19 | `c0a75b928e56` |
| [CollaborationHandoff](CollaborationHandoff.cfg) | PASS / 0 | 12,300 | 18 | `f5d65c2788b9` |
| [ContextUsage](ContextUsage.cfg) | PASS / 0 | 1,106 | 11 | `12988a7197cb` |
| [CurrentInput-Clients](CurrentInput-Clients.cfg) | PASS / 0 | 312 | 12 | `f14e705971a2` |
| [CurrentInput](CurrentInput.cfg) | PASS / 0 | 422 | 14 | `f14e705971a2` |
| [ExecLifecycle-Shell](ExecLifecycle-Shell.cfg) | **COUNTEREXAMPLE / 13** | 486 | temporal trace | `a76e2ce02a50` |
| [ExecLifecycle-ShellWake](ExecLifecycle-ShellWake.cfg) | **COUNTEREXAMPLE / 13** | 486 | temporal trace | `a76e2ce02a50` |
| [ExecLifecycle](ExecLifecycle.cfg) | PASS / 0 | 624 | 14 | `a76e2ce02a50` |
| [FileReads](FileReads.cfg) | PASS / 0 | 292 | 6 | `886b224e243a` |
| [GoalLifecycle](GoalLifecycle.cfg) | PASS / 0 | 140 | 8 | `62a243b8cf89` |
| [HookScheduling](HookScheduling.cfg) | PASS / 0 | 438 | 14 | `ae2d79cda439` |
| [HooksValidity](HooksValidity.cfg) | PASS / 0 | 199,444 | 16 | `0472d8f6399e` |
| [InputQueue](InputQueue.cfg) | PASS / 0 | 4,312 | 10 | `6859a4c39ba9` |
| [InputRecovery](InputRecovery.cfg) | PASS / 0 | 170 | 10 | `f8947296d357` |
| [LoopContinuation](LoopContinuation.cfg) | PASS / 0 | 232 | 9 | `325cdf2f1101` |
| [LoopProtocol](LoopProtocol.cfg) | PASS / 0 | 1,078 | 21 | `59c44ec31304` |
| [ModelOutput](ModelOutput.cfg) | PASS / 0 | 3,225 | 14 | `5ad522c67dc8` |
| [NestedSettlement](NestedSettlement.cfg) | PASS / 0 | 118 | 10 | `24c3e7326053` |
| [PermissionState](PermissionState.cfg) | PASS / 0 | 42,766 | 17 | `b768fc4f5dd2` |
| [Preparation](Preparation.cfg) | PASS / 0 | 140 | 9 | `ec45c0230fea` |
| [PromptAdoption](PromptAdoption.cfg) | PASS / 0 | 22,176 | 33 | `c39b5573990c` |
| [PromptRetirement](PromptRetirement.cfg) | PASS / 0 | 8,820 | 13 | `9b5db8c8bad4` |
| [RecoveryBoundary](RecoveryBoundary.cfg) | PASS / 0 | 9,021 | 13 | `21ef7ebae361` |
| [RecoverySelection](RecoverySelection.cfg) | PASS / 0 | 1,229 | 13 | `559399222ba5` |
| [RequestCuts](RequestCuts.cfg) | PASS / 0 | 201 | 14 | `6013dbcc9490` |
| [RetryBudget](RetryBudget.cfg) | PASS / 0 | 2,148 | 10 | `29a05c1b80e2` |
| [SessionBroadcast](SessionBroadcast.cfg) | PASS / 0 | 153 | 12 | `ce5fa528925f` |
| [ToolBinding](ToolBinding.cfg) | PASS / 0 | 96 | 7 | `1b3d0a0cb130` |
| [TreeLifecycle](TreeLifecycle.cfg) | PASS / 0 | 64 | 9 | `73fa55a9521b` |
| [UsageIdentity](UsageIdentity.cfg) | PASS / 0 | 96 | 7 | `24dc23e44e16` |
| [WaitRuntime](WaitRuntime.cfg) | PASS / 0 | 66,336 | 18 | `eb0b1922df1d` |
| [ApprovalOrdering](../approval-model/ApprovalOrdering.cfg) | PASS / 0 | 539,616 | 21 | `f5276ad6efa4` |
| [ToolSegments](../tool-model/ToolSegments.cfg) | PASS / 0 | 5,464 | 10 | `c84a4d32a0de` |

## Interpretation

- Shell `RecoveredSettled`: a committed start can survive a host crash without a new generation writing its terminal fact.
- Shell `RecoveredWoken`: a terminal fact can survive a host crash without its associated wake input ever being posted.
- The BackgroundExec configuration includes its actual historical recovery entry and passes under successful storage and fair recovery scheduling. The Shell configurations omit that entry because the inspected minimal profile has no such owner; no runtime code was removed for this experiment.
- The traces are model counterexamples supported by source correspondence. They do not claim a real process-crash/restart reproduction; that remains in [#1633](https://github.com/arcships/dim/issues/1633).

## Evidence boundary

Runtime code is unchanged. The previously recorded 660 relevant flow-test passes (and one pre-existing ignored long-command case) remain applicable; they were not rerun merely for these model/document edits. See [the per-part analysis](../timeline-capability-analysis.md) and [#1629](https://github.com/arcships/dim/issues/1629). Model checking is additional evidence, not a replacement for implementation correspondence or flow tests.

The final run includes the existing ApprovalOrdering and ToolSegments specimens. Each JSON record contains the exact module/configuration hashes, observed exit status, counts and TLC fingerprint/seed. The traces needed to review the failed properties are committed here. Local raw logs are not repository artifacts; the README reproduction command writes fresh logs to temporary paths. Counterexample choice can change with the fingerprint/seed.

No runtime contract, accepted ADR, persistence format or mandatory development/CI requirement was changed.
