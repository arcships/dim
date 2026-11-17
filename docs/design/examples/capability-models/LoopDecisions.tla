-------------------- MODULE LoopDecisions --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == \E budget,helpful,forbidden,started,continued,interrupted,driven,limit,calls,failed,reload\in BOOLEAN: s=[budget|->budget,helpful|->helpful,forbidden|->forbidden,started|->started,continued|->continued, interrupted|->interrupted,driven|->driven,limit|->limit,calls|->calls,failed|->failed,reload|->reload, recovery|->FALSE,result|->"none",kind|->"none"]
Failure == /\ s.result="none" /\ s'=[s EXCEPT !.kind="failure", !.result=CASE s.budget /\ ~s.recovery->"budgetRecovery" [] s.helpful /\ ~s.forbidden /\ s.started /\ ~s.continued /\ ~s.interrupted /\ ~s.driven->"continuation" [] s.limit /\ ~s.interrupted->"outputLimit" [] OTHER->"failed"]
Completed == /\ s.result="none" /\ s'=[s EXCEPT !.kind="completed", !.result=CASE s.failed->"failed" [] s.interrupted->"cancelled" [] s.calls->"tools" [] s.driven->"next" [] OTHER->"completed"]
Cancelled == /\ s.result="none" /\ s'=[s EXCEPT !.kind="cancelled", !.result=IF ~s.reload /\ ~s.interrupted /\ s.driven THEN "next" ELSE IF s.reload THEN "failed" ELSE "cancelled"]
Recover == /\ s.result="budgetRecovery" /\ s'=[s EXCEPT !.result="none", !.recovery=TRUE]
Next == Failure\/Completed\/Cancelled\/Recover
Spec == Init /\ [][Next]_s
ContinuationEligibility == s.result="continuation"=>s.helpful /\ ~s.forbidden /\ s.started /\ ~s.continued /\ ~s.interrupted /\ ~s.driven
ToolsBeforeNewInput == s.kind="completed" /\ s.calls /\ ~s.failed /\ ~s.interrupted=>s.result="tools"
NoSecondBudgetRecovery == s.recovery=>s.result#"budgetRecovery"
ReloadCannotContinueCancelled == s.kind="cancelled" /\ s.reload=>s.result="failed"
=============================================================================
