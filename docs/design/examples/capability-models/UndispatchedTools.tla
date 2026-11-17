-------------------- MODULE UndispatchedTools --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Ids == 1..2
Init == s=[started|-><<>>,complete|->{},dispatched|->{},stopping|->FALSE,settled|-><<>>,ended|->FALSE]
Start(i) == /\ ~s.stopping /\ i\notin {s.started[n]:n\in 1..Len(s.started)} /\ s'=[s EXCEPT !.started=Append(@,i)]
Complete(i) == /\ ~s.stopping /\ i\in {s.started[n]:n\in 1..Len(s.started)} /\ s'=[s EXCEPT !.complete=@\cup{i}]
Dispatch == /\ ~s.stopping /\ s.complete#{} /\ s'=[s EXCEPT !.dispatched=s.complete]
Stop == s'=[s EXCEPT !.stopping=TRUE]
Settle == /\ s.stopping /\ ~s.ended /\ s'=[s EXCEPT !.settled=SelectSeq(s.started,LAMBDA i:i\in s.complete\s.dispatched), !.ended=TRUE]
Next == (\E i\in Ids:Start(i)\/Complete(i))\/Dispatch\/Stop\/Settle
Spec == Init /\ [][Next]_s
OnlyUndispatchedCompleteCalls == s.ended=>{s.settled[n]:n\in 1..Len(s.settled)}=s.complete\s.dispatched
StartedOrderPreserved == s.ended=>s.settled=SelectSeq(s.started,LAMBDA i:i\in s.complete\s.dispatched)
=============================================================================
