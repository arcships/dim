-------------------- MODULE ToolDispatch --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Calls == {<<1,1>>,<<1,2>>,<<2,1>>}
Init == s=[claimed|->{},committed|->{},dispatched|->{},released|->{},resolved|->[c\in Calls|->0],
 approved|->[c\in Calls|->0],executed|->[c\in Calls|->0],started|->{},completed|->{},batchEnded|->{},progress|->{},cancelled|->{},failed|->{}]
Claim(r) == /\ r\notin s.released /\ s'=[s EXCEPT !.claimed=@\cup{c\in Calls:c[1]=r}]
Commit(r,ok) == /\ {c\in Calls:c[1]=r}\subseteq s.claimed /\ r\notin s.released /\ r\notin s.committed /\ r\notin s.failed
 /\ s'=[s EXCEPT !.committed=IF ok THEN @\cup{r} ELSE @, !.failed=IF ok THEN @ ELSE @\cup{r}]
Release(r) == /\ r\notin s.dispatched \/ r\in s.batchEnded
 /\ s'=[s EXCEPT !.claimed={c\in s.claimed:c[1]#r}, !.released=@\cup{r}]
Dispatch(r) == /\ r\in s.committed\(s.released\cup s.failed) /\ s'=[s EXCEPT !.dispatched=@\cup{r}]
Resolve(c,value) == /\ c[1]\in s.dispatched /\ s.resolved[c]=0
 /\ s'=[s EXCEPT !.resolved[c]=value]
Approve(c) == /\ s.resolved[c]#0 /\ s.approved[c]=0 /\ c\notin s.cancelled
 /\ s'=[s EXCEPT !.approved[c]=s.resolved[c]]
Run(c) == /\ s.approved[c]#0 /\ c\notin s.cancelled /\ c\notin s.completed
 /\ s'=[s EXCEPT !.executed[c]=s.approved[c], !.started=@\cup{c}]
Progress(c) == /\ c\in s.started\s.completed /\ s'=[s EXCEPT !.progress=@\cup{c}]
Complete(c) == /\ c[1]\in s.dispatched /\ (c\in s.started\cup s.cancelled)
 /\ s'=[s EXCEPT !.completed=@\cup{c}]
Cancel(c) == s'=[s EXCEPT !.cancelled=@\cup{c}]
EndBatch(r) == /\ r\in s.dispatched /\ {c\in Calls:c[1]=r}\subseteq s.completed
 /\ s'=[s EXCEPT !.batchEnded=@\cup{r}]
Next == (\E r\in 1..2,b\in BOOLEAN:Commit(r,b))\/(\E r\in 1..2:Claim(r)\/Release(r)\/Dispatch(r)\/EndBatch(r))
 \/(\E c\in Calls,v\in 1..2:Resolve(c,v))\/(\E c\in Calls:Approve(c)\/Run(c)\/Progress(c)\/Complete(c)\/Cancel(c))
Spec == Init /\ [][Next]_s
SameApprovedCallExecutes == \A c\in Calls:s.executed[c]#0=>s.executed[c]=s.approved[c] /\ s.approved[c]=s.resolved[c]
ProgressHasStartedIdentity == s.progress\subseteq s.started
BatchAfterEachCompletion == \A r\in s.batchEnded:{c\in Calls:c[1]=r}\subseteq s.completed
ReleasedDropsClaims == \A c\in s.claimed:c[1]\notin s.released
ClaimBeforeCommit == [][\A r\in 1..2,b\in BOOLEAN:Commit(r,b)=>{c\in Calls:c[1]=r}\subseteq s.claimed]_s
NoRunAfterObservedCancel == [][\A c\in Calls:Run(c)=>c\notin s.cancelled]_s
=============================================================================
