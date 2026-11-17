-------------------- MODULE HooksValidity --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[seq |-> 0, consumedOperator |-> 0, interrupt |-> 0, trigger |-> 0,
 phase |-> "idle", turnStarted |-> FALSE, startupWritten |-> FALSE, startupRuns |-> 0,
 writes |-> <<>>, followups |-> 0, round |-> 0, restored |-> FALSE, commands |-> 0]
Event(kind) == /\ s.seq<4
 /\ LET n==s.seq+1 IN s'=[s EXCEPT !.seq=n,
 !.consumedOperator=IF kind="consumed" THEN n ELSE @,
 !.interrupt=IF kind="interrupt" THEN n ELSE @,
 !.turnStarted=@\/(kind="turn"), !.restored=FALSE]
Trigger == /\ s.phase="idle" /\ ~s.restored /\ s.seq>0
 /\ s'=[s EXCEPT !.phase="pending", !.trigger=s.seq]
Run == /\ s.phase="pending" /\ s.commands<3
 /\ s'=[s EXCEPT !.phase="running", !.commands=@+1]
Finish(blocks) == /\ s.phase="running"
 /\ LET valid==s.consumedOperator<=s.trigger /\ s.interrupt<=s.trigger
 IN s'=[s EXCEPT !.phase="idle",
 !.writes=IF valid THEN Append(@,[trigger|->s.trigger,operator|->s.consumedOperator,interrupt|->s.interrupt]) ELSE @,
 !.followups=IF valid /\ blocks /\ s.round<3 THEN @+1 ELSE @,
 !.round=IF valid /\ blocks /\ s.round<3 THEN @+1 ELSE @]
Startup == /\ ~s.turnStarted /\ ~s.startupWritten
 /\ s'=[s EXCEPT !.startupWritten=TRUE, !.startupRuns=@+1]
Reload == s'=[s EXCEPT !.phase="idle", !.trigger=0, !.restored=TRUE]
Next == (\E k\in{"queued","consumed","interrupt","turn"}:Event(k))\/Trigger\/Run
 \/(\E b\in BOOLEAN:Finish(b))\/Startup\/Reload
Spec == Init /\ [][Next]_s
ValidOutput == \A i\in 1..Len(s.writes):s.writes[i].operator<=s.writes[i].trigger /\ s.writes[i].interrupt<=s.writes[i].trigger
StartupAtMostOnce == s.startupRuns<=1
BoundedFollowup == s.round<=3 /\ s.followups<=3
NoReplayOnReload == [][s.restored /\ s.restored' => s.commands'=s.commands]_s
=============================================================================
