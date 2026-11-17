-------------------- MODULE BranchLine --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Targets == 1..2
Init == s=[cursor |-> [t\in Targets|->0], parentEnd |-> 0, held |-> [t\in Targets|->0],
 blocks |-> <<>>, turn |-> FALSE, steered |-> FALSE, idleNotices |-> 0,
 generation |-> 0, recoveryEnd |-> FALSE, roster |-> {}, directory |-> {}]
Directory(t) == s'=[s EXCEPT !.directory=@\cup{t}]
Refresh == s'=[s EXCEPT !.roster=s.directory]
ParentEnd == /\ s.parentEnd<2 /\ s'=[s EXCEPT !.parentEnd=@+1]
Publish(t) == /\ s.parentEnd>s.cursor[t] /\ s.held[t]=0 /\ Len(s.blocks)<4
 /\ s'=[s EXCEPT !.held[t]=s.parentEnd,
 !.blocks=Append(@,[target|->t,from|->s.cursor[t],through|->s.parentEnd])]
Read(t) == /\ s.held[t]>0
 /\ s'=[s EXCEPT !.cursor[t]=s.held[t], !.held[t]=0]
Withdraw(t) == s'=[s EXCEPT !.held[t]=0]
Start == /\ ~s.turn /\ s.idleNotices<2 /\ s'=[s EXCEPT !.turn=TRUE, !.steered=FALSE, !.recoveryEnd=FALSE]
Steer(ok) == /\ s.turn /\ s'=[s EXCEPT !.steered=IF ok THEN TRUE ELSE @]
End == /\ s.turn /\ s'=[s EXCEPT !.turn=FALSE, !.idleNotices=IF s.steered THEN @ ELSE @+1]
Restore == /\ s.generation=0
 /\ s'=[s EXCEPT !.generation=1, !.turn=FALSE, !.steered=FALSE, !.recoveryEnd=TRUE, !.roster=s.directory]
Next == (\E t\in Targets:Directory(t)\/Publish(t)\/Read(t)\/Withdraw(t))\/Refresh\/ParentEnd\/Start\/(\E b\in BOOLEAN:Steer(b))\/End\/Restore
Spec == Init /\ [][Next]_s
ForwardBlocks == \A i\in 1..Len(s.blocks):s.blocks[i].from<s.blocks[i].through
CursorBound == \A t\in Targets:s.cursor[t]<=s.parentEnd
HeldAhead == \A t\in Targets:s.held[t]>0 => s.held[t]>s.cursor[t]
RestoreNoIdleNotice == [][Restore => s.idleNotices'=s.idleNotices]_s
CursorIsolation == [][\A t\in Targets:Read(t) => \A other\in Targets\{t}:s.cursor'[other]=s.cursor[other]]_s
=============================================================================
