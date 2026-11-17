-------------------- MODULE Preparation --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[phase |-> "idle", id |-> 0, cancelled |-> FALSE, calls |-> {}, ends |-> {},
 compaction |-> "none", released |-> TRUE, closing |-> FALSE]
Accept(i) == /\ ~s.closing /\ s.phase="idle" /\ i\notin(s.calls\cup s.ends)
 /\ s'=[s EXCEPT !.phase="preparing", !.id=i, !.cancelled=FALSE, !.released=FALSE]
BeginCompaction == /\ s.phase="preparing" /\ ~s.released /\ ~s.cancelled /\ s.compaction="none"
 /\ s'=[s EXCEPT !.compaction="running"]
CompactionResult == /\ s.compaction="running" /\ s'=[s EXCEPT !.compaction="ended"]
Cancel == /\ ~s.released /\ s.phase="preparing" /\ s'=[s EXCEPT !.cancelled=TRUE]
Close == s'=[s EXCEPT !.closing=TRUE, !.cancelled=IF s.released THEN @ ELSE TRUE]
Release == /\ s.phase="preparing" /\ s.compaction#"running" /\ s'=[s EXCEPT !.released=TRUE]
Finish(ok) == /\ s.phase="preparing" /\ s.released
 /\ s'=[s EXCEPT !.phase="idle", !.compaction="none",
 !.calls=IF ok /\ ~s.cancelled THEN @\cup{s.id} ELSE @,
 !.ends=IF ~ok \/ s.cancelled THEN @\cup{s.id} ELSE @]
Recover == /\ s.closing /\ s.phase="idle" /\ s'=[s EXCEPT !.closing=FALSE]
Next == (\E i\in 1..2:Accept(i))\/BeginCompaction\/CompactionResult\/Cancel\/Close\/Release\/(\E b\in BOOLEAN:Finish(b))\/Recover
Spec == Init /\ [][Next]_s
HandoffExclusive == s.calls\cap s.ends={}
OneWork == s.phase="idle" => s.released
CompactionBeforeRelease == s.compaction="running" => ~s.released
NoCallAfterObservedFailure == [][s.cancelled => s.calls'\subseteq s.calls]_s
=============================================================================
