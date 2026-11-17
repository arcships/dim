-------------------- MODULE GoalProgress --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == \E judge\in{"model","evaluator"},limit\in{0,2}:s=[status|->"paused",running|->FALSE,
 judge|->judge,limit|->limit,execution|->0,turn|->0,open|->FALSE,ended|->FALSE,
 outcome|->"none",pending|->FALSE,tasks|->FALSE,count|->0,idle|->0,automatic|->FALSE,
 progress|->FALSE,decided|->{},evaluation|-><<0,0>>,report|->"none",
 objective|->1]
Resume == /\ s.execution<2
 /\ s'=[s EXCEPT !.execution=@+1, !.status="active", !.running=TRUE, !.count=0, !.idle=0,
 !.evaluation= <<0,0>>, !.report="none", !.ended=FALSE, !.open=FALSE]
Edit == s'=[s EXCEPT !.objective=2]
Pending(b) == s'=[s EXCEPT !.pending=b]
Task(b) == s'=[s EXCEPT !.tasks=b]
Start(auto) == /\ s.running /\ ~s.open /\ s.turn<4
 /\ s'=[s EXCEPT !.turn=@+1, !.open=TRUE, !.ended=FALSE, !.automatic=auto, !.progress=FALSE,
 !.count=IF auto THEN @ ELSE 0, !.idle=IF auto THEN @ ELSE 0, !.report="none", !.evaluation= <<0,0>>]
Tool == /\ s.open /\ s'=[s EXCEPT !.progress=TRUE]
End(outcome) == /\ s.open
 /\ s'=[s EXCEPT !.open=FALSE, !.ended=TRUE, !.outcome=outcome]
Stop == s'=[s EXCEPT !.running=FALSE, !.status="paused", !.evaluation= <<0,0>>, !.report="none"]
Reload == s'=[s EXCEPT !.running=FALSE, !.status=IF s.status="active" THEN "paused" ELSE @,
 !.count=0, !.idle=0, !.evaluation= <<0,0>>, !.report="none", !.ended=FALSE, !.open=FALSE]
Fork == s'=[s EXCEPT !.running=FALSE, !.status="none", !.evaluation= <<0,0>>, !.report="none", !.ended=FALSE, !.open=FALSE]
AskEvaluation == /\ s.running /\ s.judge="evaluator" /\ s.ended /\ s.outcome="completed"
 /\ <<s.execution,s.turn>>\notin s.decided /\ s.evaluation= <<0,0>>
 /\ s'=[s EXCEPT !.evaluation= <<s.execution,s.turn>>]
Report(e,t,r) == /\ e\in 1..2 /\ t\in 1..4
 /\ s'=[s EXCEPT !.report=IF s.running /\ <<e,t>>=s.evaluation THEN r ELSE @]
Conclude == /\ s.running /\ s.judge="evaluator" /\ s.report\in{"done","blocked"}
 /\ s'=[s EXCEPT !.status=IF s.report="done" THEN "completed" ELSE "blocked", !.running=FALSE]
Decide == /\ s.running /\ s.ended /\ ~s.open /\ ~s.tasks
 /\ <<s.execution,s.turn>>\notin s.decided
 /\ ((s.outcome="failed") \/ (s.outcome="completed" /\ ~s.pending /\ (s.judge="model" \/ s.report="continue")))
 /\ LET idle==IF s.judge="evaluator" \/ ~s.automatic \/ s.progress THEN 0 ELSE s.idle+1
        go==s.outcome="completed" /\ (s.limit=0 \/ s.count<s.limit) /\ idle<3
 IN s'=[s EXCEPT !.decided=@\cup{<<s.execution,s.turn>>}, !.count=IF go THEN @+1 ELSE @,
 !.idle=idle, !.running=go, !.status=IF go THEN "active" ELSE "paused"]
ModelConclude(status) == /\ s.running /\ s.judge="model" /\ status\in{"completed","blocked"}
 /\ s'=[s EXCEPT !.running=FALSE, !.status=status]
Next == (\E status\in{"completed","blocked"}:ModelConclude(status))\/Resume\/Edit\/(\E b\in BOOLEAN:Pending(b)\/Task(b)\/Start(b))\/Tool
 \/(\E o\in{"completed","failed","cancelled"}:End(o))\/Stop\/Reload\/Fork\/AskEvaluation
 \/(\E e\in 1..2,t\in 1..4,r\in{"continue","done","blocked"}:Report(e,t,r))\/Conclude\/Decide
Spec == Init /\ [][Next]_s
NoBusyContinuation == [][Decide /\ s.running'=>~s.pending /\ ~s.tasks]_s
ValidContinuation == [][Decide /\ s.running'=>s.outcome="completed" /\ s.idle'<3 /\ (s.limit=0 \/ s.count<s.limit)]_s
OneDecisionPerTurn == [][Decide=><<s.execution,s.turn>>\notin s.decided /\ s.decided'=s.decided\cup{<<s.execution,s.turn>>}]_s
EditPreservesState == [][Edit=>s.status'=s.status /\ s.running'=s.running]_s
StaleReportIgnored == [][\A e\in 1..2,t\in 1..4,r\in{"continue","done","blocked"}:Report(e,t,r)/\<<e,t>>#s.evaluation=>s.report'=s.report /\ s.status'=s.status]_s
ReloadCannotContinue == [][Reload=>~s.running' /\ s.evaluation'= <<0,0>>]_s
=============================================================================
