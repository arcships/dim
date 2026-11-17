-------------------- MODULE SnapshotRefresh --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
CONSTANT Retry
VARIABLE s
Init == s=[external|->0,sampled|->0,last|->2,wake|->TRUE,pending|->TRUE,watch|->FALSE,
 active|->FALSE,closing|->FALSE,phase|->"idle",writes|-><<>>,stable|->FALSE,changes|->0,generation|->0]
Listen == s'=[s EXCEPT !.watch=TRUE]
Activate == /\ s.watch /\ ~s.closing /\ s'=[s EXCEPT !.active=TRUE]
Change(v) == /\ ~s.stable /\ s.changes<2
 /\ s'=[s EXCEPT !.external=v, !.wake=@\/s.watch, !.changes=@+1]
Read == /\ s.active /\ ~s.closing /\ s.phase="idle" /\ (s.wake\/s.pending)
 /\ s'=[s EXCEPT !.phase="reading", !.wake=FALSE, !.pending=FALSE]
ReadOk == /\ s.phase="reading" /\ ~s.closing
 /\ s'=[s EXCEPT !.phase="sampled", !.sampled=s.external]
ReadFail == /\ Retry /\ ~s.stable /\ s.phase="reading" /\ ~s.closing
 /\ s'=[s EXCEPT !.phase="idle", !.pending=TRUE]
AppendFail == /\ ~s.stable /\ s.phase="sampled" /\ ~s.closing
 /\ s'=[s EXCEPT !.phase="idle", !.pending=Retry]
Commit == /\ s.phase="sampled" /\ ~s.closing
 /\ s'=[s EXCEPT !.phase="idle", !.last=s.sampled,
 !.writes=IF s.sampled=s.last THEN @ ELSE Append(@,[generation|->s.generation,value|->s.sampled])]
Stable == s'=[s EXCEPT !.stable=TRUE]
Close == s'=[s EXCEPT !.closing=TRUE, !.phase="idle"]
Reload == /\ s.closing /\ s.generation=0
 /\ s'=[s EXCEPT !.generation=1, !.last=2, !.active=FALSE, !.watch=FALSE, !.wake=TRUE,
 !.pending=TRUE, !.closing=FALSE]
Next == Listen\/Activate\/(\E v\in 0..1:Change(v))\/Read\/ReadOk\/ReadFail\/AppendFail\/Commit\/Stable\/Close\/Reload
Spec == Init /\ [][Next]_s /\ WF_s(Read) /\ WF_s(ReadOk) /\ WF_s(Commit)
ListenerBeforeRead == s.phase#"idle"=>s.watch /\ s.active
NoDuplicateWithinGeneration == \A i\in 2..Len(s.writes):s.writes[i].generation=s.writes[i-1].generation=>s.writes[i].value#s.writes[i-1].value
LastIsCommitted == s.last#2=>Len(s.writes)>0 /\ s.writes[Len(s.writes)]=[generation|->s.generation,value|->s.last]
FailurePreservesLast == [][ReadFail\/AppendFail=>s.last'=s.last /\ s.writes'=s.writes]_s
ClosePreventsWrite == [][s.closing=>s.writes'=s.writes]_s
ReloadForgetsLast == [][Reload=>s.last'=2 /\ s.pending']_s
EventuallyCurrent == s.stable /\ s.active /\ Retry ~> s.last=s.external \/ s.closing
=============================================================================
