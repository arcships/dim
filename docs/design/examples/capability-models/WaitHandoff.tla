-------------------- MODULE WaitHandoff --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == \E surface\in{"tool","native","face"},timeout\in{0,1,300000,300001},reason\in{"notification","steer","timeout","interrupted","alreadyWaiting","reloading","drop"}:
 s=[surface|->surface,timeout|->timeout,reason|->reason,phase|->"input",entered|->FALSE,
 response|->"none",result|->"none",reloadMarker|->FALSE,
 cancelledTurn|->FALSE,turnFailed|->FALSE,nextRequest|->FALSE]
Validate == /\ s.phase="input"
 /\ LET invalid==s.surface="tool" /\ (s.timeout=0 \/ s.timeout>300000)
 IN s'=[s EXCEPT !.phase=IF invalid THEN "returned" ELSE "waiting", !.entered=~invalid,
 !.response=IF invalid THEN "argumentError" ELSE @]
Finish == /\ s.phase="waiting"
 /\ s'=[s EXCEPT !.phase="returned", !.response=CASE s.reason="drop"->"noReply"
 [] s.reason="reloading"->IF s.surface="tool" THEN "hostReloadCancelled" ELSE "reloading"
 [] s.reason="alreadyWaiting"->IF s.surface="tool" THEN "toolError" ELSE "alreadyWaiting"
 [] OTHER->s.reason]
Settle == /\ s.surface="tool" /\ s.phase="returned" /\ s.response#"noReply"
 /\ s'=[s EXCEPT !.phase="settled", !.reloadMarker=(s.response="hostReloadCancelled"),
 !.result=IF s.response\in{"argumentError","toolError","hostReloadCancelled"} THEN "error" ELSE "text"]
InterruptTurn == s'=[s EXCEPT !.cancelledTurn=TRUE]
Consume == /\ s.phase="settled"
 /\ s'=[s EXCEPT !.phase="consumed", !.turnFailed=s.reloadMarker,
 !.nextRequest=~s.reloadMarker /\ ~s.cancelledTurn]
Next == Validate\/Finish\/Settle\/InterruptTurn\/Consume
Spec == Init /\ [][Next]_s
ToolTimeoutBoundary == s.surface="tool" /\ (s.timeout=0 \/ s.timeout>300000)=>~s.entered
ServiceTimeoutIsNotToolBound == s.surface#"tool" /\ s.phase#"input"=>s.entered
ReloadFailsTurn == s.phase="consumed" /\ s.reason="reloading" /\ s.entered=>s.turnFailed /\ ~s.nextRequest
OrdinaryWaitResultIsText == s.phase\in{"settled","consumed"} /\ s.entered /\ s.reason\in{"timeout","interrupted","notification","steer"}=>s.result="text" /\ ~s.reloadMarker
NoReplyForDrop == s.entered /\ s.reason="drop" /\ s.phase="returned"=>s.response="noReply"
NoContinuationWhenConsumedInterrupted == [][Consume /\ s.cancelledTurn=>~s.nextRequest']_s
=============================================================================
