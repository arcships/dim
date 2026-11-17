-------------------- MODULE CurrentInput --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
CONSTANT Retry
VARIABLE s
Init == s=[external |-> 0, sampled |-> 0, last |-> 2, wake |-> TRUE, pending |-> FALSE,
 active |-> FALSE, closing |-> FALSE, phase |-> "idle", writes |-> <<>>, failed |-> FALSE,
 stable |-> FALSE, changes |-> 0]
Activate == /\ ~s.active /\ ~s.closing /\ s'=[s EXCEPT !.active=TRUE]
Change(v) == /\ ~s.stable /\ s.changes<2
 /\ s'=[s EXCEPT !.external=v, !.wake=TRUE, !.changes=@+1]
Read == /\ s.active /\ ~s.closing /\ s.phase="idle" /\ (s.wake\/(Retry /\ s.pending))
 /\ s'=[s EXCEPT !.sampled=s.external, !.phase="read", !.wake=FALSE]
CommitInput == /\ s.phase="read" /\ ~s.closing
 /\ s'=[s EXCEPT !.phase="idle", !.pending=FALSE,
 !.writes=IF s.sampled=s.last THEN @ ELSE Append(@,s.sampled), !.last=s.sampled]
Fail == /\ s.phase="read" /\ ~s.stable /\ ~s.failed
 /\ s'=[s EXCEPT !.phase="idle", !.pending=Retry, !.failed=TRUE]
Stabilize == /\ ~s.stable /\ s'=[s EXCEPT !.stable=TRUE]
Close == /\ ~s.closing /\ s'=[s EXCEPT !.closing=TRUE, !.phase="idle"]
Next == Activate\/(\E v\in 0..1:Change(v))\/Read\/CommitInput\/Fail\/Stabilize\/Close
Spec == Init /\ [][Next]_s /\ WF_s(Read) /\ WF_s(CommitInput)
NoConsecutiveDuplicates == \A i\in 2..Len(s.writes):s.writes[i]#s.writes[i-1]
NoWriteBeforeActivation == ~s.active => s.writes= << >>
LastMeansCommitted == Len(s.writes)>0 => s.last=s.writes[Len(s.writes)]
NoWriteAfterClosing == [][s.closing => s.writes'=s.writes]_s
EventuallyCurrent == (s.stable /\ s.active /\ Retry) ~> (s.last=s.external \/ s.closing)
=============================================================================
