-------------------- MODULE ModelStream --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Keys == {<<1,1>>,<<1,2>>,<<2,1>>}
Init == s=[started|-><<>>,complete|->{},ended|->{},closures|->[r\in 1..2|-><<>>],outcome|->[r\in 1..2|->"none"],usage|-><<>>,terminal|-><<>>,executed|->{}]
Start(k) == /\ k[1]\notin s.ended /\ k\notin {s.started[n]:n\in 1..Len(s.started)}
 /\ s'=[s EXCEPT !.started=Append(@,k)]
Complete(k) == /\ k[1]\notin s.ended /\ k\in {s.started[n]:n\in 1..Len(s.started)}
 /\ s'=[s EXCEPT !.complete=@\cup{k}]
Usage(r,n) == /\ r\notin s.ended /\ Len(s.usage)<2
 /\ s'=[s EXCEPT !.usage=Append(@,[request|->r,value|->n])]
End(r,ok) == /\ r\notin s.ended
 /\ LET calls==SelectSeq(s.started,LAMBDA k:k[1]=r)
 valid==ok /\ {calls[n]:n\in 1..Len(calls)}\subseteq s.complete
 IN s'=[s EXCEPT !.ended=@\cup{r}, !.outcome[r]=IF valid THEN "completed" ELSE "failed",
 !.closures[r]=IF valid THEN <<>> ELSE [n\in 1..Len(calls)|->[key|->calls[n],reason|->IF calls[n]\in s.complete THEN "notExecuted" ELSE "incompleteArguments"]],
 !.terminal=Append(@,[request|->r,closed|->IF valid THEN {} ELSE {calls[n]:n\in 1..Len(calls)}])]
Run(k) == /\ s.outcome[k[1]]="completed" /\ k\in s.complete /\ s'=[s EXCEPT !.executed=@\cup{k}]
Next == (\E k\in Keys:Start(k)\/Complete(k)\/Run(k))\/(\E r\in 1..2,b\in BOOLEAN:End(r,b))\/(\E r,n\in 1..2:Usage(r,n))
Spec == Init /\ [][Next]_s
NoPartialExecution == s.executed\subseteq s.complete /\ \A k\in s.executed:s.outcome[k[1]]="completed"
FailureClosesAllStarted == \A r\in 1..2:s.outcome[r]="failed"=>{s.closures[r][n].key:n\in 1..Len(s.closures[r])}={k\in {s.started[n]:n\in 1..Len(s.started)}:k[1]=r}
ClosureKeepsIdentityAndMeaning == \A r\in 1..2: \A n\in 1..Len(s.closures[r]):s.closures[r][n].key[1]=r /\ (s.closures[r][n].reason="incompleteArguments"<=>s.closures[r][n].key\notin s.complete)
TerminalUnique == Len(s.terminal)=Cardinality(s.ended)
UsageAfterEndForbidden == [][\A r,n\in 1..2:Usage(r,n)=>r\notin s.ended]_s
=============================================================================
