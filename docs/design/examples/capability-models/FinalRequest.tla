-------------------- MODULE FinalRequest --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[object|->1,counter|->1,limits|->1,projection|->1,cut|->1,compaction|->0,
 phase|->"building",estimateObject|->1,estimateCounter|->1,estimateLimits|->1,
 validatedObject|->0,validatedCounter|->0,validatedLimits|->0,reused|->FALSE,
 saved|->FALSE,savedProjection|->0,savedAtCompaction|->0,called|->FALSE,overBudget|->FALSE,
 compacted|->FALSE,failed|->FALSE]
Transform == /\ s.phase="building" /\ s.object=1
 /\ s'=[s EXCEPT !.object=2, !.projection=0]
Counter == /\ s.phase="building" /\ s'=[s EXCEPT !.counter=2]
Limits == /\ s.phase="building" /\ s'=[s EXCEPT !.limits=2]
Budget(over) == /\ s.phase="building" /\ s'=[s EXCEPT !.overBudget=over]
Compact == /\ s.compaction=0 /\ s'=[s EXCEPT !.compaction=2]
RecoverBudget(fits) == /\ s.phase="building" /\ s.overBudget /\ ~s.compacted
 /\ s'=[s EXCEPT !.compacted=TRUE, !.overBudget=~fits, !.failed=~fits,
 !.phase=IF fits THEN "building" ELSE "failed"]
RejectRemainingBudget == /\ s.phase="building" /\ s.compacted /\ s.overBudget
 /\ s'=[s EXCEPT !.phase="failed", !.failed=TRUE]
Validate == /\ s.phase="building" /\ ~s.overBudget /\ ~s.failed
 /\ s'=[s EXCEPT !.phase="validated", !.validatedObject=s.object,
 !.validatedCounter=s.counter, !.validatedLimits=s.limits,
 !.reused=(s.object=s.estimateObject /\ s.counter=s.estimateCounter /\ s.limits=s.estimateLimits)]
InitializeUsage == /\ s.phase="validated" /\ s.projection=1 /\ s.compaction<=s.cut
 /\ s'=[s EXCEPT !.saved=TRUE, !.savedProjection=s.projection, !.savedAtCompaction=s.compaction]
Call == /\ s.phase="validated"
 /\ s'=[s EXCEPT !.phase="called", !.called=TRUE]
Next == RejectRemainingBudget\/Transform\/Counter\/Limits\/(\E b\in BOOLEAN:Budget(b)\/RecoverBudget(b))\/Compact\/Validate\/InitializeUsage\/Call
Spec == Init /\ [][Next]_s
ActualRequestValidated == s.called=>s.validatedObject=s.object /\ s.validatedCounter=s.counter /\ s.validatedLimits=s.limits
ReuseOnlySameEvidence == s.reused=>s.object=s.estimateObject /\ s.counter=s.estimateCounter /\ s.limits=s.estimateLimits
SaveRetainedProjectionOnly == s.saved=>s.savedProjection=1 /\ s.savedAtCompaction<=s.cut
NoOverBudgetInvocation == s.called=>~s.overBudget /\ ~s.failed
FailedRecoveryStops == s.failed=>~s.called /\ s.phase="failed"
CompactionDefersInitialization == [][s.compaction>s.cut=>s.saved'=s.saved]_s
=============================================================================
