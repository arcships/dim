-------------------- MODULE LoopInputs --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Ids == 1..3
Init == s=[inputs|-><<>>,modes|->[i\in Ids|->"queue"],claimed|->{},interruptedAt|->0,
 phase|->"idle",requests|-><<>>,recovery|->FALSE,stopped|->FALSE]
Pending == SelectSeq(s.inputs,LAMBDA i:i\notin s.claimed)
Driving == {i\in Ids:i\in {Pending[n]:n\in 1..Len(Pending)} /\ s.modes[i]#"append" /\ i>s.interruptedAt}
Input(mode) == /\ Len(s.inputs)<3
 /\ LET i==Len(s.inputs)+1 IN s'=[s EXCEPT !.inputs=Append(@,i), !.modes[i]=mode]
Begin == /\ s.phase="idle" /\ Driving#{}
 /\ s'=[s EXCEPT !.phase="ready", !.stopped=FALSE]
Compact == /\ s.phase="ready" /\ ~s.recovery /\ s'=[s EXCEPT !.phase="compacting", !.recovery=TRUE]
CompactEnd == /\ s.phase="compacting" /\ s'=[s EXCEPT !.phase=IF s.stopped THEN "idle" ELSE "ready"]
Request(budgetSource) == /\ s.phase="ready" /\ ~s.stopped /\ Len(s.requests)<2
 /\ (~budgetSource \/ Len(s.requests)>0)
 /\ LET chosen==IF budgetSource THEN <<>> ELSE Pending
 IN s'=[s EXCEPT !.requests=Append(@,[inputs|->chosen,budget|->budgetSource]),
 !.claimed=@\cup{chosen[n]:n\in 1..Len(chosen)}, !.phase="running"]
Finish == /\ s.phase="running" /\ s'=[s EXCEPT !.phase=IF ~s.stopped /\ Driving#{} THEN "ready" ELSE "idle"]
Interrupt == s'=[s EXCEPT !.interruptedAt=Len(s.inputs), !.stopped=TRUE,
 !.phase=IF s.phase="compacting" THEN @ ELSE "idle"]
Next == (\E m\in{"queue","append","steer"}:Input(m))\/Begin\/Compact\/CompactEnd
 \/(\E b\in BOOLEAN:Request(b))\/Finish\/Interrupt
Spec == Init /\ [][Next]_s
InputClaimedAtMostOnce == \A a,b\in 1..Len(s.requests):a#b=>
 {s.requests[a].inputs[n]:n\in 1..Len(s.requests[a].inputs)}\cap{s.requests[b].inputs[n]:n\in 1..Len(s.requests[b].inputs)}={}
BudgetSourceClaimsNoNewInputs == \A n\in 1..Len(s.requests):s.requests[n].budget=>s.requests[n].inputs= <<>>
OldQueueCannotWake == [][Begin=>Driving#{}]_s
ClaimIncludesOrderedPending == [][Request(FALSE)=>s.requests'[Len(s.requests')].inputs=Pending]_s
CompactionDefersRequests == [][s.phase="compacting"=>s.requests'=s.requests]_s
=============================================================================
