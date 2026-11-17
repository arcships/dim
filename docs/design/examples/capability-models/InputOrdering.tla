-------------------- MODULE InputOrdering --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Ids == 1..3
Init == s=[order|-><<>>,mode|->[i\in Ids|->"none"],owner|->[i\in Ids|->0],held|->{},released|->{},deleted|->{},consumed|->{},turn|->0,turns|->0,starting|->FALSE,selected|->0]
Ordinary == {i\in s.held\cup s.released:s.mode[i]#"handover"}
HeldQueue == SelectSeq(s.order,LAMBDA i:i\in s.held /\ s.mode[i]="queue")
Arrive(i,m,t) == /\ i\notin {s.order[n]:n\in 1..Len(s.order)}
 /\ LET stale==m="handover" /\ (s.turn=0 \/ t#s.turn)
 release==m\in{"steer","append"} \/ (m="queue" /\ s.turn=0 /\ ~s.starting /\ HeldQueue= <<>>)
 IN s'=[s EXCEPT !.order=Append(@,i), !.mode[i]=m, !.owner[i]=t,
 !.deleted=IF stale THEN @\cup{i} ELSE @, !.released=IF release THEN @\cup{i} ELSE @,
 !.held=IF ~stale /\ ~release THEN @\cup{i} ELSE @,
 !.starting=@\/(release /\ m#"append" /\ s.turn=0)]
Start == /\ s.starting /\ s.turn=0 /\ s.turns<2
 /\ s'=[s EXCEPT !.turns=@+1, !.turn=s.turns+1, !.starting=FALSE]
End == /\ s.turn#0
 /\ LET expired=={i\in s.held:s.mode[i]="handover" /\ s.owner[i]=s.turn}
 next==IF HeldQueue= <<>> THEN 0 ELSE Head(HeldQueue)
 IN s'=[s EXCEPT !.held=@\(expired\cup{next}), !.deleted=@\cup expired,
 !.released=IF next=0 THEN @ ELSE @\cup{next}, !.selected=next, !.turn=0, !.starting=(next#0)]
Consume(i) == /\ i\in s.released \/ (i\in s.held /\ s.mode[i]="handover" /\ s.owner[i]=s.turn)
 /\ s'=[s EXCEPT !.consumed=@\cup{i}, !.released=@\{i}, !.held=@\{i}]
Withdraw(i) == /\ i\in s.held /\ s'=[s EXCEPT !.held=@\{i}, !.deleted=@\cup{i}]
Stop == s'=[s EXCEPT !.starting=FALSE]
Next == (\E i\in Ids,m\in{"queue","steer","append","handover"},t\in 1..2:Arrive(i,m,t))\/Start\/End\/Stop\/(\E i\in Ids:Consume(i)\/Withdraw(i))
Spec == Init /\ [][Next]_s
NoStaleHandover == \A i\in s.held:s.mode[i]="handover"=>s.turn#0 /\ s.owner[i]=s.turn
NoHandoverInPublicPending == \A i\in Ordinary:s.mode[i]#"handover"
NoDeletedConsumption == s.deleted\cap s.consumed={}
QueueArrivalOrder == [][End /\ HeldQueue# <<>>=>s.selected'=Head(HeldQueue)]_s
NoNewQueueOvertakesHeld == [][\A i\in Ids,t\in 1..2:Arrive(i,"queue",t) /\ HeldQueue# <<>>=>i\in s.held']_s
=============================================================================
