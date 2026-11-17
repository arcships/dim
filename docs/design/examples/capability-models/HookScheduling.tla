-------------------- MODULE HookScheduling --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Ids == 1..2
Init == s=[queued |-> <<>>, active |-> 0, handlers |-> {}, finished |-> {}, results |-> {},
 ordinaryDone |-> {}, approval |-> "none", approvalWrites |-> 0, cancelled |-> FALSE,
 accepted |-> {}, restored |-> FALSE]
Enqueue(i) == /\ ~s.restored /\ i\notin s.accepted
 /\ s'=[s EXCEPT !.queued=Append(@,i), !.accepted=@\cup{i}]
Start == /\ ~s.restored /\ s.active=0 /\ Len(s.queued)>0
 /\ s'=[s EXCEPT !.active=Head(s.queued), !.queued=Tail(@), !.handlers=Ids, !.finished={}, !.results={}]
Handler(i,ok) == /\ s.active#0 /\ i\in s.handlers\s.finished
 /\ s'=[s EXCEPT !.finished=@\cup{i}, !.results=IF ok THEN @\cup{i} ELSE @]
Finish == /\ s.active#0 /\ s.finished=s.handlers
 /\ s'=[s EXCEPT !.active=0, !.ordinaryDone=@\cup{s.active}]
Approval == /\ ~s.restored /\ s.approval="none" /\ s'=[s EXCEPT !.approval="running"]
ApprovalEnded == /\ s.approval="running" /\ s'=[s EXCEPT !.approval="cancelled", !.cancelled=TRUE]
ApprovalOutput == /\ s.approval="running" /\ ~s.cancelled
 /\ s'=[s EXCEPT !.approval="done", !.approvalWrites=@+1]
Reload == /\ ~s.restored
 /\ s'=[s EXCEPT !.restored=TRUE, !.queued= << >>, !.active=0, !.approval="cancelled", !.cancelled=TRUE]
Next == (\E i\in Ids:Enqueue(i)\/(\E b\in BOOLEAN:Handler(i,b)))\/Start\/Finish\/Approval\/ApprovalEnded\/ApprovalOutput\/Reload
Spec == Init /\ [][Next]_s
NoUnfinishedMerge == s.results\subseteq s.finished
DoneWasAccepted == s.ordinaryDone\subseteq s.accepted
NoDuplicateOrdinary == s.active#0 => s.active\notin s.ordinaryDone
ApprovalIndependent == [][ApprovalEnded=>s.active'=s.active /\ s.queued'=s.queued]_s
NoCancelledApprovalOutput == [][s.cancelled=>s.approvalWrites'=s.approvalWrites]_s
NoReplayAfterReload == [][s.restored=>s.ordinaryDone'=s.ordinaryDone /\ s.approvalWrites'=s.approvalWrites]_s
=============================================================================
