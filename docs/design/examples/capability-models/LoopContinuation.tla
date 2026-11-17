-------------------- MODULE LoopContinuation --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[phase |-> "model", continued |-> FALSE, budgetRetried |-> FALSE, automatic |-> 0,
 compact |-> FALSE, interrupted |-> FALSE, driver |-> FALSE, chain |-> 1,
 deferred |-> 0, resumedChain |-> 0, selectedChain |-> 0, emptyTools |-> FALSE,
 outcome |-> "none"]
Drive == s'=[s EXCEPT !.driver=TRUE]
Interrupt == s'=[s EXCEPT !.interrupted=TRUE]
QueueCompaction == s'=[s EXCEPT !.compact=TRUE]
Failure(budget,helpful,announced,forbidden,limit) == /\ s.phase\in{"model","continuation"}
 /\ LET recover==budget /\ ~s.budgetRetried
        again==~recover /\ helpful /\ announced /\ ~forbidden /\ ~s.continued /\ ~s.interrupted /\ ~s.driver
 IN s'=[s EXCEPT
 !.phase=IF s.interrupted THEN "ended" ELSE IF recover THEN "model" ELSE IF again THEN (IF s.compact THEN "deferred" ELSE "continuation") ELSE "ended",
 !.budgetRetried=@\/recover, !.continued=@\/again, !.automatic=IF again THEN @+1 ELSE @,
 !.deferred=IF again /\ s.compact THEN s.chain ELSE @,
 !.selectedChain=IF again THEN s.chain ELSE @,
 !.resumedChain=IF again /\ ~s.compact THEN s.chain ELSE @,
 !.emptyTools=@\/again,
 !.outcome=IF s.interrupted THEN "cancelled" ELSE IF recover \/ again THEN "none" ELSE IF limit THEN "output_limit" ELSE "failed"]
CompactionResult == /\ s.phase="deferred"
 /\ s'=[s EXCEPT !.compact=FALSE, !.phase=IF s.interrupted THEN "ended" ELSE "continuation",
 !.resumedChain=IF s.interrupted THEN @ ELSE s.deferred,
 !.outcome=IF s.interrupted THEN "cancelled" ELSE @]
ContinuationEnd == /\ s.phase="continuation" /\ s'=[s EXCEPT !.phase="ended", !.outcome="completed"]
Next == Drive\/Interrupt\/QueueCompaction\/(\E b,h,a,f,l\in BOOLEAN:Failure(b,h,a,f,l))\/CompactionResult\/ContinuationEnd
Spec == Init /\ [][Next]_s
OneAutomaticContinuation == s.automatic<=1
DeferredKeepsChain == s.phase="continuation"=>s.resumedChain=s.selectedChain /\ s.selectedChain>0
ContinuationHasNoTools == s.phase="continuation"=>s.emptyTools
NoAutomaticOnDrivenTurn == [][s.driver=>s.automatic'=s.automatic]_s
NoAutomaticAfterInterrupt == [][s.interrupted=>s.automatic'=s.automatic]_s
=============================================================================
