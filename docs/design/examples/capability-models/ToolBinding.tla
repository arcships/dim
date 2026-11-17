-------------------- MODULE ToolBinding --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[registry |-> 1, retired |-> {}, prepared |-> 0, declaration |-> 0,
 bound |-> 0, selected |-> 0, ran |-> 0, empty |-> FALSE, failed |-> FALSE]
Replace == /\ s.registry<3 /\ s'=[s EXCEPT !.retired=@\cup{s.registry}, !.registry=@+1]
Prepare(empty) == /\ s.prepared=0
 /\ s'=[s EXCEPT !.prepared=s.registry, !.declaration=IF empty THEN 0 ELSE s.registry, !.empty=empty]
Select(id) == /\ s.prepared#0 /\ s.bound=0 /\ s.selected=0 /\ s'=[s EXCEPT !.selected=id]
Bind == /\ s.selected#0 /\ s.bound=0
 /\ s'=[s EXCEPT !.bound=IF s.selected=s.declaration /\ ~s.empty THEN s.prepared ELSE 0,
 !.failed=(s.selected#s.declaration \/ s.empty)]
Run == /\ s.bound#0 /\ s.ran=0
 /\ s'=[s EXCEPT !.ran=IF s.bound\in s.retired THEN 0 ELSE s.bound, !.failed=(s.bound\in s.retired)]
Next == Replace\/(\E b\in BOOLEAN:Prepare(b))\/(\E i\in 1..3:Select(i))\/Bind\/Run
Spec == Init /\ [][Next]_s
PinnedIdentity == s.bound#0 => s.bound=s.prepared /\ s.bound=s.declaration
NoReplacementExecution == s.ran#0 => s.ran=s.prepared
EmptyMeansNoTool == s.empty => s.bound=0 /\ s.ran=0
NoUndeclaredBinding == s.bound#0 => s.selected=s.declaration
=============================================================================
