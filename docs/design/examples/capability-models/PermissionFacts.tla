-------------------- MODULE PermissionFacts --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[version|->0,policy|->"allow",sampleVersion|->0,samplePolicy|->"allow",phase|->"sample",
 fact|->"unknown",result|->"none",decisionVersion|->0,decisionPolicy|->"none",cancelled|->FALSE]
Tighten == /\ s.version=0 /\ s'=[s EXCEPT !.version=1, !.policy="deny"]
Sample == /\ s.phase="sample" /\ ~s.cancelled
 /\ s'=[s EXCEPT !.sampleVersion=s.version, !.samplePolicy=s.policy,
 !.phase=IF s.policy="deny" THEN "done" ELSE "reading", !.result=IF s.policy="deny" THEN "deny" ELSE "none",
 !.decisionVersion=IF s.policy="deny" THEN s.version ELSE @, !.decisionPolicy=IF s.policy="deny" THEN s.policy ELSE @]
Facts(f) == /\ s.phase="reading" /\ ~s.cancelled /\ f\in{"workspace","outside","system","owned","error"}
 /\ s'=[s EXCEPT !.phase="read", !.fact=f]
Decide == /\ s.phase="read" /\ ~s.cancelled
 /\ LET stale==s.sampleVersion#s.version /\ s.fact#"error"
        result==CASE s.fact\in{"system","owned","error"}->"deny" [] s.fact="outside"->"ask" [] OTHER->s.samplePolicy
 IN s'=[s EXCEPT !.phase=IF stale THEN "sample" ELSE "done", !.result=IF stale THEN "none" ELSE result,
 !.decisionVersion=IF stale THEN @ ELSE s.version, !.decisionPolicy=IF stale THEN @ ELSE s.policy]
Cancel == /\ s.phase#"done" /\ s'=[s EXCEPT !.cancelled=TRUE, !.phase="done", !.result="cancelled"]
Next == Tighten\/Sample\/(\E f\in{"workspace","outside","system","owned","error"}:Facts(f))\/Decide\/Cancel
Spec == Init /\ [][Next]_s
TightPolicyWinsAtDecision == s.result="allow"=>s.decisionPolicy="allow"
FactsErrorCannotAdmit == s.fact="error" /\ s.phase="done"=>s.result\in{"deny","cancelled"}
CancelCannotAdmit == s.cancelled=>s.result="cancelled"
VersionChangeRetries == [][Decide /\ s.sampleVersion#s.version /\ s.fact#"error"=>s.phase'="sample" /\ s.result'="none"]_s
ProtectedWritesDenied == s.phase="done" /\ s.fact\in{"system","owned"}=>s.result\in{"deny","cancelled"}
=============================================================================
