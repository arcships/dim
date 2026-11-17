-------------------- MODULE ExecStopHistory --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Ids == 1..2
Terminal == {"completed","failed","cancelled"}
Init == s=[status|->[i\in Ids|->"absent"],winner|->[i\in Ids|->"none"],released|->{},cancel|->{},wake|->{},owed|->{},closing|->FALSE,reopened|->FALSE,
 endedWhileOpen|->{},starts|->{},reply|->[i\in Ids|->"none"]]
Start(i) == /\ ~s.closing /\ s.status[i]="absent"
 /\ s'=[s EXCEPT !.status[i]="running", !.starts=@\cup{i}]
Stop(i) == /\ s.status[i]="running" /\ s'=[s EXCEPT !.cancel=@\cup{i}]
Observe(i,outcome) == /\ s.status[i]="running" /\ s.winner[i]="none"
 /\ (outcome#"cancelled" \/ i\in s.cancel)
 /\ s'=[s EXCEPT !.winner[i]=outcome]
Release(i,ok) == /\ s.winner[i]#"none" /\ i\notin s.released
 /\ s'=[s EXCEPT !.released=@\cup{i}, !.winner[i]=IF ok THEN @ ELSE "failed"]
End(i) == /\ s.status[i]="running" /\ i\in s.released
 /\ LET outcome==s.winner[i] IN
 s'=[s EXCEPT !.status[i]=outcome, !.owed=IF outcome#"cancelled" /\ ~s.closing THEN @\cup{i} ELSE @,
 !.endedWhileOpen=IF ~s.closing THEN @\cup{i} ELSE @]
Wake(i) == /\ i\in s.owed /\ s'=[s EXCEPT !.wake=@\cup{i}]
Close == s'=[s EXCEPT !.closing=TRUE, !.cancel=@\cup{i\in Ids:s.status[i]="running"}]
Reopen == /\ s.closing /\ ~s.reopened /\ (\A i\in Ids:s.status[i]#"running") /\ s'=[s EXCEPT !.closing=FALSE, !.reopened=TRUE]
Read(i,timeout,bad) == s'=[s EXCEPT !.reply[i]=IF bad THEN "readError" ELSE CASE s.status[i]="absent"->"unknown"
 [] s.status[i]="running"->IF timeout THEN "timeout" ELSE "waiting" [] OTHER->s.status[i]]
StopEnded(i) == /\ s.status[i]\in Terminal /\ Read(i,FALSE,FALSE)
Next == (\E i\in Ids:Start(i)\/Stop(i)\/End(i)\/Wake(i)\/StopEnded(i))
 \/(\E i\in Ids,o\in Terminal:Observe(i,o))\/(\E i\in Ids,b\in BOOLEAN:Release(i,b))
 \/(\E i\in Ids,t,b\in BOOLEAN:Read(i,t,b))\/Close\/Reopen
Spec == Init /\ [][Next]_s
WakeHasEligibleEnd == s.wake\subseteq s.owed /\ \A i\in s.owed:s.status[i]\in{"completed","failed"} /\ i\in s.endedWhileOpen
ReleaseBeforeEnd == \A i\in Ids:s.status[i]\in Terminal=>i\in s.released
TerminalIsStable == [][\A i\in Ids:s.status[i]\in Terminal=>s.status'[i]=s.status[i]]_s
HistoricalCallsDoNotRestart == [][\A i\in Ids,t,b\in BOOLEAN:Read(i,t,b)=>s.starts'=s.starts /\ s.status'=s.status /\ s.wake'=s.wake]_s
ReopeningDoesNotRequalifyOldEnd == [][Reopen=>s.owed'=s.owed]_s
LateStopPreservesWinner == [][\A i\in Ids:Stop(i)=>s.winner'[i]=s.winner[i]]_s
=============================================================================
