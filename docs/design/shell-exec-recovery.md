# Shell exec recovery

ShellSession owns its branch's PTY, command queue, foreground-to-background handoff and process cleanup.
It writes the existing `exec/started` and `exec/ended` facts. BackgroundExec owns recovery and completion
notices for that shared exec protocol; its [query, fork boundary and notice contract](../architecture/rust-agent/plugins/WorkspaceTools.md#backgroundexec)
applies to facts from either execution producer. Event types do not have an exclusive writer.

## Assembly and handoff

The minimal-execution profile mounts BackgroundExec alongside ShellSession. ShellSession declares a
load-order dependency on `backgroundExec`: recovery finishes before a new shell can accept commands,
and a reload of the recovery owner first unloads its shell dependent. ShellSession releases its old
processes and writes their terminal facts before BackgroundExec is removed. A ShellSession-only reload
does not restart the recovery owner or mistake another producer's live work for abandoned history.
The local-execution profile already provides BackgroundExec, so a host adding ShellSession there reuses it.

ShellSession appends terminals but does not append wake inputs. The Dispatcher delivers terminals to
BackgroundExec, which applies the existing status and branch-closing rules and writes the tagged notice.
For a terminal without a native BackgroundExec task, it reads that task's description from its exact
`exec/started` fact; it does not build a second task table or take ownership of a live PTY.
The existing ShellSession stop port continues to stop its own live tasks. BackgroundExec's live capture
and native-process stop operations are not a new PTY control interface.

## Recovery cases

| Committed prefix at restoration | Required outcome |
| --- | --- |
| Started, no ended | One failed terminal stating that the old execution result is unknown; no command restart |
| Eligible ended, no tagged notice | Keep the terminal and post the missing notice |
| Ended and tagged notice | Neither a second terminal nor a second notice |
| Cancelled terminal | No completion wake |
| Facts inherited across the Session fork boundary | No adoption of copied external work |

Recovery uses BackgroundExec's structured selection at a fixed cutoff. It does not replay command
execution, scan completed history into a cache, or change the persistent event format.
Publication, failure handling and retry on the next generation remain the owner's existing contract.

## Rule check

Both branch parts are Capabilities under principles §1. ShellSession owns I/O and terminal production;
BackgroundExec owns recovery/notification decisions for the exec protocol. This changes the profile's
assembly and removes a duplicate notification writer, not the meaning of an exec event.

- Principles §4 rules 1, 2 and 7: the terminal is the handoff; neither part mutates the other's state.
- Rule 3: no before callback.
- Rules 4 and 8: existing Timeline queries, Dispatcher and dependency lifecycle; no mechanism change.
- Rule 5: existing separate terminal/notice commits remain valid, including recovery of that gap.
- Rule 6: notices use the existing input adoption path.
- Rule 9: no new event or UI fact; every retained fact drives a later recovery or notification decision.
- ADR 002 D1–D3: existing branch owners and a declared load-order dependency; no wiring part or core dependency on ShellSession.
- D4: no new setting. D5: no new service method, wire field or binding protocol.

## Requirements raised to mechanism owners

None. Existing query, publication and lifecycle contracts suffice.

## Verification

The minimal-execution flow restores disposable storage containing an unfinished start, an unreported
completed end and a cancelled end. It checks failed settlement, preservation of the existing result,
notice eligibility, each notice's stored command description and no duplicates on another restoration. These are committed-prefix recovery
cases, not a claim that the test killed a real host process. Live background and occupying-command flows also check the completion notice.
Stop and shell-exit flows check terminal outcomes; the close flow checks one terminal per admitted task.
