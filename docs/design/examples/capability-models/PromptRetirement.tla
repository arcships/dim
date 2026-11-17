-------------------- MODULE PromptRetirement --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Ids == 1..2
Init == s=[active |-> Ids, entries |-> {}, saved |-> {}, retired |-> {}, history |-> <<>>,
 changed |-> 0, loaded |-> TRUE, accepted |-> {}, checkpoint |-> 0]
Change(i,system,delivered) == /\ s.loaded /\ s.changed<3
 /\ LET adopt==(system\/delivered)/\i\in s.active IN
 s'=[s EXCEPT !.changed=@+1, !.entries=IF adopt THEN @\cup{i} ELSE @,
 !.accepted=IF adopt THEN @\cup{i} ELSE @]
ScopeEnd(i) == s'=[s EXCEPT !.active=@\{i}]
Checkpoint == /\ s.loaded /\ Len(s.history)<8
 /\ LET gone==s.entries\s.active
        next==s.entries\cap s.active
 IN s'=[s EXCEPT !.entries=next, !.saved=next, !.retired=@\cup gone, !.checkpoint=s.changed,
 !.history=Append(Append(@,[kind|->"retire",ids|->gone]),[kind|->"checkpoint",ids|->next])]
Crash == /\ s.loaded /\ s'=[s EXCEPT !.loaded=FALSE, !.entries={}]
Restore == /\ ~s.loaded /\ s'=[s EXCEPT !.loaded=TRUE, !.entries=s.saved]
Next == (\E i\in Ids,b,d\in BOOLEAN:Change(i,b,d))\/(\E i\in Ids:ScopeEnd(i))\/Checkpoint\/Crash\/Restore
Spec == Init /\ [][Next]_s
NoUndeliveredEntry == s.entries\subseteq s.accepted
RetiredNotRestored == s.entries\cap s.retired={}
CheckpointPair == Len(s.history)%2=0
ResultLast == \A i\in 1..Len(s.history):s.history[i].kind="checkpoint" => i>1 /\ s.history[i-1].kind="retire" /\ s.history[i].ids\cap s.history[i-1].ids={}
SourceNeverRewinds == [][s.changed'>=s.changed]_s
=============================================================================
