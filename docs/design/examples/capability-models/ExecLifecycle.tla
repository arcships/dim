-------------------- MODULE ExecLifecycle --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
CONSTANT RecoverHistory
VARIABLE s
Ids == 1..2
Init == s=[started |-> {}, live |-> {}, released |-> {}, ended |-> {}, wake |-> {},
 eligible |-> {}, holds |-> {}, crashed |-> FALSE, restored |-> FALSE, closing |-> FALSE,
 inherited |-> {}, stopped |-> FALSE]
Start(i) == /\ ~s.crashed /\ ~s.restored /\ ~s.closing /\ i\notin s.started
 /\ s'=[s EXCEPT !.started=@\cup{i}, !.live=@\cup{i}, !.holds=@\cup{i}]
Release(i) == /\ ~s.crashed /\ i\in s.live
 /\ s'=[s EXCEPT !.live=@\{i}, !.released=@\cup{i}]
End(i) == /\ ~s.restored /\ ~s.crashed /\ i\in s.released\s.ended
 /\ s'=[s EXCEPT !.ended=@\cup{i}, !.holds=@\{i},
 !.eligible=IF s.closing THEN @ ELSE @\cup{i}]
Wake(i) == /\ ~s.crashed /\ i\in s.eligible\s.wake
 /\ (RecoverHistory \/ ~s.restored)
 /\ s'=[s EXCEPT !.wake=@\cup{i}]
StopTurn == s'=[s EXCEPT !.stopped=TRUE]
Close == /\ ~s.crashed /\ s'=[s EXCEPT !.closing=TRUE]
Crash == /\ ~s.crashed /\ ~s.restored
 /\ s'=[s EXCEPT !.crashed=TRUE, !.live={}, !.holds={}]
Restore == /\ s.crashed
 /\ s'=[s EXCEPT !.crashed=FALSE, !.restored=TRUE]
Recover(i) == /\ RecoverHistory /\ s.restored /\ i\in s.started\(s.ended\cup s.inherited)
 /\ s'=[s EXCEPT !.ended=@\cup{i}, !.eligible=IF s.closing THEN @ ELSE @\cup{i}]
Fork == /\ s.crashed
 /\ s'=[s EXCEPT !.inherited=s.started, !.eligible={}, !.released={}]
Next == (\E i\in Ids:Start(i)\/Release(i)\/End(i)\/Wake(i)\/Recover(i))\/StopTurn\/Close\/Crash\/Restore\/Fork
Spec == Init /\ [][Next]_s /\ WF_s(Restore) /\ (\A i\in Ids:WF_s(Recover(i))/\WF_s(Wake(i)))
WakeHasTerminal == s.wake\subseteq s.ended
TerminalHasStart == s.ended\subseteq s.started
LiveHasHold == s.live\subseteq s.holds
ReleaseBeforeTerminal == ~s.restored /\ ~s.crashed => s.ended\subseteq s.released
NoWakeFromInheritedRecovery == s.eligible\cap s.inherited={}
StopKeepsWork == [][StopTurn => s.live'=s.live]_s
RecoveredSettled == s.restored ~> (s.started\s.inherited)\subseteq s.ended
RecoveredWoken == s.restored ~> s.eligible\subseteq s.wake
=============================================================================
