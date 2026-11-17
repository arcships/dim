-------------------- MODULE TreeLifecycle --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Nodes == 1..2
Parent(i) == IF i=1 THEN 0 ELSE 1
Init == s=[phase |-> [i\in Nodes|->"absent"], closing |-> {}, results |-> {}, accepted |-> {},
 published |-> {}, rootClosing |-> FALSE, endOrder |-> <<>>, ownerRecoveries |-> 0]
ParentOpen(i) == IF Parent(i)=0 THEN ~s.rootClosing ELSE s.phase[Parent(i)]="open" /\ Parent(i)\notin s.closing
Reserve(i) == /\ s.phase[i]="absent" /\ ParentOpen(i)
 /\ s'=[s EXCEPT !.phase[i]="reserved", !.accepted=@\cup{i}]
Build(i,ok) == /\ s.phase[i]="reserved"
 /\ LET yes==ok /\ ParentOpen(i) /\ i\notin s.closing IN
 s'=[s EXCEPT !.phase[i]=IF yes THEN "open" ELSE "failed", !.results=@\cup{i},
 !.published=IF yes THEN @\cup{i} ELSE @]
RequestClose(i) == /\ s.phase[i]\in{"reserved","open"}
 /\ s'=[s EXCEPT !.closing=@\cup{n\in Nodes:n>=i}]
CloseRoot == s'=[s EXCEPT !.rootClosing=TRUE, !.closing=Nodes]
End(i) == /\ s.phase[i]="open" /\ i\in s.closing
 /\ (\A n\in Nodes:Parent(n)=i => s.phase[n]\in{"absent","failed","ended"})
 /\ s'=[s EXCEPT !.phase[i]="ended", !.endOrder=Append(@,i)]
RestoreOwner == /\ s.ownerRecoveries=0
 /\ s'=[s EXCEPT !.ownerRecoveries=1,
 !.phase=[i\in Nodes|->IF s.phase[i]="reserved" THEN "failed" ELSE s.phase[i]],
 !.results=@\cup{i\in Nodes:s.phase[i]="reserved"}]
Next == (\E i\in Nodes:Reserve(i)\/(\E b\in BOOLEAN:Build(i,b))\/RequestClose(i)\/End(i))\/CloseRoot\/RestoreOwner
Spec == Init /\ [][Next]_s
ResultRequiresReservation == s.results\subseteq s.accepted
PublicationRequiresResult == s.published\subseteq s.results
NoOrphanOpenChild == s.phase[2]="open" => s.phase[1]="open"
ChildrenEndFirst == s.phase[1]="ended" => s.phase[2]\in{"absent","failed","ended"}
SingleResult == \A i\in Nodes:s.phase[i]="failed" => i\notin s.published
NoPublicationAfterClose == [][s.rootClosing => s.published'=s.published]_s
=============================================================================
