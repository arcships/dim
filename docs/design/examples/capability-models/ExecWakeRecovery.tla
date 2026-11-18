-------------------- MODULE ExecWakeRecovery --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Ids == {1,2}
Rows(log) == {log[i]:i \in 1..Len(log)}
Init == \E log \in {<<>>,<<1>>}: s=[log|->log,durable|->Len(log),todo|->{},
 phase|->"reading",fault|->FALSE,generation|->0]
Capture == /\ s.phase="reading"
 /\ s'=[s EXCEPT !.todo=Ids\Rows(s.log),!.phase="posting"]
Post(i) == /\ s.phase="posting" /\ i\in s.todo
 /\ s'=[s EXCEPT !.todo=@\{i},!.log=IF s.fault THEN @ ELSE Append(@,i)]
Persist == /\ ~s.fault /\ s.durable<Len(s.log)
 /\ s'=[s EXCEPT !.durable=@+1]
Fail == /\ s.generation=0 /\ ~s.fault /\ s.durable<Len(s.log)
 /\ s'=[s EXCEPT !.fault=TRUE]
Done == /\ s.phase="posting" /\ s.todo={}
 /\ s'=[s EXCEPT !.phase="done"]
Reload == /\ s.phase="done" /\ s.generation=0
 /\ (s.fault \/ s.durable=Len(s.log))
 /\ s'=[s EXCEPT !.log=SubSeq(@,1,s.durable),!.generation=1,!.fault=FALSE,!.phase="reading"]
Next == Capture \/ Persist \/ Fail \/ Done \/ Reload \/ (\E i\in Ids:Post(i))
Spec == Init /\ [][Next]_s /\ WF_s(Capture) /\ WF_s(Persist) /\ WF_s(Done) /\ WF_s(Reload)
 /\ (\A i\in Ids:WF_s(Post(i)))
Committed == SubSeq(s.log,1,s.durable)
NoDuplicateCommittedWake == Len(Committed)=Cardinality(Rows(Committed))
OnlyEligibleWakes == Rows(s.log)\subseteq Ids
FailureStopsPersistence == [][s.fault /\ s.generation'=s.generation => s.durable'=s.durable]_s
RetryEventuallyPosts == <> (Rows(Committed)=Ids)
=============================================================================
