-------------------- MODULE FileReads --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[clock |-> 0, established |-> 0, bound |-> FALSE, cuts |-> <<>>,
 decision |-> "none", witness |-> 0, generation |-> 0]
Read == /\ s.clock<3 /\ s'=[s EXCEPT !.clock=@+1, !.established=s.clock+1, !.bound=TRUE, !.decision="none"]
Compact(cut,success) == /\ s.clock<3 /\ cut<=s.clock
 /\ s'=[s EXCEPT !.clock=@+1, !.cuts=IF success THEN Append(@,[at|->s.clock+1,retain|->cut]) ELSE @, !.decision="none"]
WriteCheck == /\ s.bound
 /\ LET lost==\E i\in 1..Len(s.cuts):s.cuts[i].at>s.established /\ s.cuts[i].retain>s.established
 IN s'=[s EXCEPT !.decision=IF lost THEN "compacted" ELSE "bound", !.witness=s.established]
Reload == /\ s.generation<1 /\ s'=[s EXCEPT !.bound=FALSE, !.established=0, !.decision="none", !.generation=@+1]
Next == Read\/(\E c\in 0..3,b\in BOOLEAN:Compact(c,b))\/WriteCheck\/Reload
Spec == Init /\ [][Next]_s
NoRestoredBinding == ~s.bound => s.established=0
NoCompactedWrite == s.decision="bound" => ~\E i\in 1..Len(s.cuts):s.cuts[i].at>s.witness /\ s.cuts[i].retain>s.witness
CompactionNeedsSuccessfulCut == s.decision="compacted" => \E i\in 1..Len(s.cuts):s.cuts[i].retain>s.witness
=============================================================================
