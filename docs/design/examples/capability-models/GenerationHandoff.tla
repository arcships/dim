-------------------- MODULE GenerationHandoff --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
CONSTANT AllowCrash
VARIABLE s
Has(k,log) == \E i \in 1..Len(log): log[i]=k
Terminal(log) == Has("reload",log) \/ Has("normal",log)
Init == s=[log|-><<"start","call">>,durable|->2,published|->2,
 wait|->"old",tool|->"old",loop|->"old",call|->"accepted",
 waitReply|->"none",result|->"none",loopCut|->0,toolCut|->0,
 loopView|->"pending",loopAt|->2,toolOpen|->FALSE,
 loopFailed|->FALSE,decision|->"none",
 invoked|->FALSE,effect|->FALSE,
 fault|->FALSE,cleanupError|->FALSE,crashed|->FALSE,crashEffect|->FALSE]
Persist == /\ s.durable < Len(s.log) /\ ~s.fault
 /\ s'=[s EXCEPT !.durable=@+1]
StorageFailure == /\ s.durable < Len(s.log) /\ ~s.fault
 /\ s'=[s EXCEPT !.fault=TRUE]
Publish == /\ s.published < s.durable /\ ~s.fault
 /\ s'=[s EXCEPT !.published=@+1]
Invoke == /\ s.tool="old" /\ s.wait="old" /\ s.call="accepted" /\ ~s.fault
 /\ s'=[s EXCEPT !.call="running",!.invoked=TRUE]
Effect == /\ s.call="running" /\ ~s.effect
 /\ s'=[s EXCEPT !.effect=TRUE]
NormalReply == /\ s.wait="old" /\ s.tool="old" /\ s.call="running"
 /\ s'=[s EXCEPT !.call="returned",!.waitReply="normal"]
CloseWait == /\ s.wait="old" /\ ~s.crashed
 /\ s'=[s EXCEPT !.wait="closing"]
ReplyReload == /\ s.wait="closing" /\ s.call \in {"accepted","running"}
 /\ s'=[s EXCEPT !.call="returned",!.waitReply="reload"]
RemoveWait == /\ s.wait="closing" /\ s.call \notin {"accepted","running"}
 /\ s'=[s EXCEPT !.wait="new"]
CloseTool == /\ s.tool="old" /\ ~s.crashed
 /\ s'=[s EXCEPT !.tool="closing"]
CancelToolCall == /\ s.tool="closing" /\ s.call \in {"accepted","running"}
 /\ s'=[s EXCEPT !.call="returned",!.waitReply="reload"]
Settle(ok) == /\ s.tool \in {"old","closing"} /\ s.call="returned" /\ s.result="none"
 /\ s'=[s EXCEPT !.result=IF ok /\ ~s.fault THEN "written" ELSE "failed",
 !.log=IF ok /\ ~s.fault THEN Append(@,IF s.waitReply="reload" \/ s.tool="closing" THEN "reload" ELSE "normal") ELSE @,
 !.fault=@ \/ ~ok,!.cleanupError=@ \/ ~ok \/ s.fault,!.call="done"]
RemoveTool == /\ s.tool="closing" /\ s.call="done"
 /\ s'=[s EXCEPT !.tool="absent"]
ReloadLoop == /\ s.loop="old" /\ ~s.crashed /\ s.decision#"continue"
 /\ s'=[s EXCEPT !.loop="absent"]
CaptureTool == /\ s.tool="absent"
 /\ s'=[s EXCEPT !.tool="rebuild",!.toolCut=Len(s.log),!.toolOpen=~Terminal(s.log)]
RebuildTool(ok) == /\ s.tool="rebuild"
 /\ LET success == ok /\ (~s.toolOpen \/ ~s.fault)
 IN s'=[s EXCEPT !.tool=IF success THEN "new" ELSE "failed",
 !.log=IF success /\ s.toolOpen THEN Append(@,"reload") ELSE @,
 !.result=IF success /\ s.toolOpen THEN "written" ELSE @]
CaptureLoop == /\ s.loop="absent"
 /\ s'=[s EXCEPT !.loop="rebuild",!.loopCut=Len(s.log),!.loopAt=Len(s.log),!.loopFailed=~Has("end",s.log),
 !.loopView=IF Has("end",s.log) THEN "ended" ELSE IF Terminal(s.log) THEN "terminal" ELSE "pending"]
RebuildLoop(ok) == /\ s.loop="rebuild"
 /\ LET settle == s.loopView="terminal"
        success == ok /\ (~settle \/ ~s.fault)
 IN s'=[s EXCEPT !.loop=IF success THEN "new" ELSE "failed",
 !.log=IF success /\ settle THEN Append(@,"end") ELSE @,
 !.loopView=IF success /\ settle THEN "ended" ELSE @,
 !.decision=IF success /\ settle THEN "failed" ELSE @]
ConsumeLoop == /\ s.loop \in {"old","new"} /\ ~s.fault /\ s.loopAt<s.published
 /\ s'=[s EXCEPT !.loopAt=@+1,
 !.loopFailed=@ \/ s.log[s.loopAt+1]="reload",
 !.loopView=CASE s.log[s.loopAt+1] \in {"normal","reload"} -> "terminal"
 [] s.log[s.loopAt+1]="end" -> "ended" [] OTHER -> @]
DecideAfterResult(ok) == /\ s.loop \in {"old","new"} /\ s.loopView="terminal" /\ ~s.fault
 /\ s'=[s EXCEPT !.log=IF ok /\ s.loopFailed THEN Append(@,"end") ELSE @,
 !.loopView=IF ok THEN IF s.loopFailed THEN "ended" ELSE "continued" ELSE @,!.fault=~ok,
 !.decision=IF ok THEN IF s.loopFailed THEN "failed" ELSE "continue" ELSE @]
Crash == /\ AllowCrash /\ ~s.crashed
 /\ s'=[s EXCEPT !.log=SubSeq(@,1,s.durable),!.published=s.durable,
 !.wait="new",!.tool="absent",!.loop="absent",!.call="lost",!.waitReply="none",
 !.result=IF Terminal(SubSeq(s.log,1,s.durable)) THEN "written" ELSE "none",
 !.loopView="pending",!.loopFailed=FALSE,!.decision="none",!.loopAt=0,!.loopCut=0,!.toolCut=0,!.toolOpen=FALSE,
 !.fault=FALSE,!.crashed=TRUE,!.crashEffect=s.effect]
Next == Persist \/ StorageFailure \/ Publish \/ Invoke \/ Effect \/ NormalReply \/ CloseWait \/ ReplyReload \/ RemoveWait
 \/ CloseTool \/ CancelToolCall \/ RemoveTool \/ ReloadLoop \/ CaptureTool \/ CaptureLoop
 \/ ConsumeLoop \/ Crash \/ (\E ok \in BOOLEAN: Settle(ok) \/ RebuildTool(ok) \/ RebuildLoop(ok) \/ DecideAfterResult(ok))
Spec == Init /\ [][Next]_s
OneTerminalPerHistory == Cardinality({i \in 1..Len(s.log):s.log[i] \in {"normal","reload"}}) <= 1
EndAfterTerminal == \A i \in 1..Len(s.log): s.log[i]="end" => Terminal(SubSeq(s.log,1,i-1))
ReloadDoesNotContinue == (Has("reload",s.log) \/ s.loopFailed) => s.decision # "continue"
NoOldWorkReexecution == [][Invoke => ~s.invoked /\ ~s.crashed]_s
RebuildDoesNotInvoke == [][\A ok \in BOOLEAN: RebuildTool(ok) => UNCHANGED <<s.invoked,s.effect>>]_s
CrashDoesNotUndoEffects == s.crashEffect => s.effect
PublicationPrefix == s.published <= s.durable /\ s.durable <= Len(s.log)
WaitRemovalDrainsCall == [][RemoveWait => s.call \notin {"accepted","running"}]_s
ToolRemovalJoinsSettlement == [][RemoveTool => s.call="done" /\ (Terminal(s.log) \/ s.cleanupError)]_s
FailedRebuildNotReady == [][(RebuildTool(FALSE) => s.tool'="failed")
 /\ (RebuildLoop(FALSE) => s.loop'="failed")]_s
=============================================================================
