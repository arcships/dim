-------------------- MODULE HookDecisions --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Ids == 1..2
Init == \E async\in SUBSET Ids,onFailure\in SUBSET Ids,outputs\in[Ids->{"success","block","failed"}]: s=[async|->async,onFailure|->onFailure,outputs|->outputs,done|->{},merged|-><<>>,blocked|->FALSE, started|->FALSE,queried|->FALSE,committed|->FALSE,cancelled|->{},replies|->{}]
Finish(i) == s'=[s EXCEPT !.done=@\cup{i}]
Merge == /\ ~s.blocked /\ Len(s.merged)<2 /\ LET i==Len(s.merged)+1 IN /\ (i\in s.async \/ i\in s.done) /\ s'=[s EXCEPT !.merged=Append(@,i), !.blocked=i\notin s.async /\ (s.outputs[i]="block" \/ (s.outputs[i]="failed" /\ i\in s.onFailure))]
Query == /\ ~s.started /\ s'=[s EXCEPT !.queried=TRUE]
TurnStart == s'=[s EXCEPT !.started=TRUE]
Write == /\ s.queried /\ ~s.started /\ ~s.blocked /\ Len(s.merged)=2 /\ s'=[s EXCEPT !.committed=TRUE]
Cancel(i) == s'=[s EXCEPT !.cancelled=@\cup{i}]
ApprovalReply(i) == /\ i\notin s.cancelled /\ s'=[s EXCEPT !.replies=@\cup{i}]
Next == (\E i\in Ids:Finish(i)\/Cancel(i)\/ApprovalReply(i))\/Merge\/Query\/TurnStart\/Write
Spec == Init /\ [][Next]_s
CatalogOrder == s.merged=SubSeq(<<1,2>>,1,Len(s.merged))
OnlySynchronousCanBlock == s.blocked=>\E i\in {s.merged[n]:n\in 1..Len(s.merged)}: i\notin s.async /\ (s.outputs[i]="block" \/ (s.outputs[i]="failed" /\ i\in s.onFailure))
SessionStartRecheckedAtWrite == [][Write=>~s.started]_s
BlockStopsLaterMerge == [][s.blocked=>s.merged'=s.merged]_s
ApprovalCancellationScoped == [][\A i\in Ids:Cancel(i)=>s.cancelled'=s.cancelled\cup{i} /\ s.replies'=s.replies]_s
CancelledApprovalCannotReply == [][\A i\in Ids:i\in s.cancelled=> (i\in s.replies' <=> i\in s.replies)]_s
=============================================================================
