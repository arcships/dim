# Second coverage-fill TLC record

All 30 configurations in this record completed with exit 0 and exhausted queues. The reused Wait FIFO record checks `FirstPendingNotice` (the singular `PROPERTY` declaration in its cfg). The 46 existing configurations retain exact-hash evidence: 44 passes and the two Shell recovery counterexamples. This accounts for the 76 capability/approval/tool example configurations in this second-fill record; it is not a coverage percentage. The separate guide's WaitRecovery example is outside this set.

Code correspondence: `ec43222ab`; relevant source hashes, model/config hashes, actual exits, seeds and properties are in [JSON](coverage-completion-results.json). Toolchain: Java 17.0.21, TLA+ tools 1.7.4 / TLC 2.19, one worker, 2 GiB heap. See [scope and correspondence](coverage-completion.md) before interpreting a pass.

| Configuration | Distinct states | Depth | Exit |
| --- | ---: | ---: | ---: |
| [ApprovalRequests](ApprovalRequests.cfg) | 289,488 | 16 | 0 |
| [ApprovalRequests-Branches](ApprovalRequests-Branches.cfg) | 277,460 | 16 | 0 |
| [BranchEligibility](BranchEligibility.cfg) | 1,341,216 | 20 | 0 |
| [BranchEligibility-CapBoundary](BranchEligibility-CapBoundary.cfg) | 720 | 10 | 0 |
| [BranchEligibility-MoveToChild](BranchEligibility-MoveToChild.cfg) | 2 | 2 | 0 |
| [BranchEligibility-AtCapBelow](BranchEligibility-AtCapBelow.cfg) | 2 | 2 | 0 |
| [BranchCapSwitch](BranchCapSwitch.cfg) | 12 | 2 | 0 |
| [BranchCapSwitch-BackToRoot](BranchCapSwitch-BackToRoot.cfg) | 2 | 2 | 0 |
| [BranchCapSwitch-OverCap](BranchCapSwitch-OverCap.cfg) | 2 | 2 | 0 |
| [BroadcastRecords](BroadcastRecords.cfg) | 3,759 | 14 | 0 |
| [CollaborationIdentity](CollaborationIdentity.cfg) | 148,360 | 22 | 0 |
| [CollaborationIdentity-Members](CollaborationIdentity-Members.cfg) | 199,784 | 22 | 0 |
| [ExecStopHistory](ExecStopHistory.cfg) | 56,114 | 18 | 0 |
| [HookDecisions](HookDecisions.cfg) | 88,992 | 12 | 0 |
| [HookReplyMeaning](HookReplyMeaning.cfg) | 1,620 | 2 | 0 |
| [InputOrdering](InputOrdering.cfg) | 168,667 | 12 | 0 |
| [InputReceipts](InputReceipts.cfg) | 1,914 | 14 | 0 |
| [LoopDecisions](LoopDecisions.cfg) | 12,288 | 4 | 0 |
| [LoopInputs](LoopInputs.cfg) | 3,142 | 12 | 0 |
| [ModelStream](ModelStream.cfg) | 21,567 | 14 | 0 |
| [OpeningConfiguration](OpeningConfiguration.cfg) | 25 | 9 | 0 |
| [OutputAvailability](OutputAvailability.cfg) | 138 | 7 | 0 |
| [PermissionComposition](PermissionComposition.cfg) | 184,320 | 3 | 0 |
| [PromptProjection](PromptProjection.cfg) | 23,686 | 8 | 0 |
| [ShellCommands](ShellCommands.cfg) | 2,327 | 15 | 0 |
| [ToolDispatch](ToolDispatch.cfg) | 94,217 | 29 | 0 |
| [ToolRetirement](ToolRetirement.cfg) | 2,145 | 11 | 0 |
| [TreeOpening](TreeOpening.cfg) | 866,144 | 23 | 0 |
| [UndispatchedTools](UndispatchedTools.cfg) | 75 | 8 | 0 |
| [UsageSources](UsageSources.cfg) | 123,864 | 14 | 0 |

Fresh checks explored 3,912,052 distinct states in total across independent configurations. State counts are not unique whole-system behaviors or requirement coverage. No symmetry or state-constraint filters were used. All final model/config files predate their completed runs. Development parser errors and stopped larger drafts are not counted as passing runs.

Five separate complete state dumps confirm the positive scenarios recorded in JSON: post-Closed Notice/Fence, old input joining a new driven request, the root as a valid move target again together with an over-cap state after switching back to it, two distinct failed tool-argument states, and execution through an old handler pin after replacement. These are reachable states, not claims that every execution reaches them.

Reproduce each configuration using the README command, selecting the module before any configuration suffix (for example `CollaborationIdentity-Members.cfg` uses `CollaborationIdentity.tla`). State directories and dumps belong outside the repository. The Shell failures remain in the original record and #1633; no runtime fix or crash reproduction is claimed.
