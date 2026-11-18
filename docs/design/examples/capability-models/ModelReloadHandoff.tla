-------------------- MODULE ModelReloadHandoff --------------------
EXTENDS Naturals, TLC
CONSTANT AllowQueryFailure
VARIABLE s
Init == s=[provider|->"absent",call|->0,cut|->0,published|->1,selected|->FALSE,
 invoked|->0,seen|->FALSE,running|->FALSE,terminals|->0,outcome|->"none",retried|->FALSE]
Head == 1 + (IF s.call=0 THEN 0 ELSE 1) + s.terminals
WriteCall == /\ s.call=0 /\ s'=[s EXCEPT !.call=2]
Capture == /\ s.provider="absent"
 /\ s'=[s EXCEPT !.provider="reading",!.cut=Head,!.selected=s.call#0 /\ s.terminals=0]
ReadOK == /\ s.provider="reading"
 /\ s'=[s EXCEPT !.provider="live",!.terminals=IF s.selected THEN @+1 ELSE @,
 !.outcome=IF s.selected THEN "recovered" ELSE @]
ReadFail == /\ AllowQueryFailure /\ ~s.retried /\ s.provider="reading"
 /\ s'=[s EXCEPT !.provider="failed"]
Retry == /\ s.provider="failed" /\ ~s.retried
 /\ s'=[s EXCEPT !.provider="absent",!.retried=TRUE]
Publish == /\ s.published<Head /\ s'=[s EXCEPT !.published=@+1]
Deliver == /\ s.provider="live" /\ s.call#0 /\ s.call>s.cut /\ s.published>=s.call /\ ~s.seen
 /\ s'=[s EXCEPT !.seen=TRUE,!.running=TRUE,!.invoked=@+1]
Finish == /\ s.running
 /\ s'=[s EXCEPT !.running=FALSE,!.terminals=@+1,!.outcome="live"]
Next == WriteCall \/ Capture \/ ReadOK \/ ReadFail \/ Retry \/ Publish \/ Deliver \/ Finish
Spec == Init /\ [][Next]_s /\ WF_s(WriteCall) /\ WF_s(Capture) /\ WF_s(ReadOK)
 /\ WF_s(Retry) /\ WF_s(Publish) /\ WF_s(Deliver) /\ WF_s(Finish)
OneTerminal == s.terminals<=1
OneInvocation == s.invoked<=1
RecoveredDoesNotInvoke == s.outcome="recovered" => s.invoked=0
LiveIsBeyondCut == s.invoked>0 => s.call>s.cut
FailedReadDoesNotInvoke == s.provider="failed" => s.invoked=0
CallEventuallyEnds == s.call#0 ~> s.terminals=1
=============================================================================
