-------------------- MODULE ShellCommands --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[queued|-><<>>,accepted|->{},cancelled|->{},active|->0,generation|->1,runGeneration|->[i\in 1..2|->0],
 task|->[i\in 1..2|->0],started|->{},terminal|->[i\in 1..2|->"none"],winner|->[i\in 1..2|->"none"],released|->{},closed|->FALSE]
Queue(i) == /\ i\notin s.accepted /\ ~s.closed
 /\ s'=[s EXCEPT !.accepted=@\cup{i}, !.queued=Append(@,i)]
CancelQueued(i) == /\ i\in {s.queued[n]:n\in 1..Len(s.queued)} /\ s'=[s EXCEPT !.cancelled=@\cup{i}]
Dequeue == /\ s.active=0 /\ s.queued# <<>>
 /\ LET i==Head(s.queued) skip==i\in s.cancelled \/ s.closed IN
 s'=[s EXCEPT !.queued=Tail(@), !.active=IF skip THEN 0 ELSE i, !.runGeneration[i]=IF skip THEN @ ELSE s.generation]
Background == /\ s.active#0 /\ s.task[s.active]=0 /\ ~s.closed
 /\ s'=[s EXCEPT !.task[s.active]=s.active, !.started=@\cup{s.active}]
Choose(g,kind) == /\ s.active#0 /\ g=s.runGeneration[s.active] /\ s.winner[s.active]="none"
 /\ s'=[s EXCEPT !.winner[s.active]=kind]
Release == /\ s.active#0 /\ s.winner[s.active]#"none"
 /\ s'=[s EXCEPT !.released=@\cup{s.active}]
Settle == /\ s.active#0 /\ s.active\in s.released
 /\ s'=[s EXCEPT !.terminal[s.active]=IF s.task[s.active]=0 THEN "foregroundOnly" ELSE s.winner[s.active], !.active=0]
Close == s'=[s EXCEPT !.closed=TRUE]
Reload == /\ s.active=0 /\ s.generation=1 /\ s.queued= <<>>
 /\ s'=[s EXCEPT !.generation=2, !.closed=FALSE]
Next == (\E i\in 1..2:Queue(i)\/CancelQueued(i))\/Dequeue\/Background
 \/(\E g\in 1..2,k\in{"completed","cancelled","failed"}:Choose(g,k))\/Release\/Settle\/Close\/Reload
Spec == Init /\ [][Next]_s
OneTaskIdentity == \A i\in s.started:s.task[i]=i
TerminalRequiresRelease == \A i\in 1..2:s.terminal[i]#"none"=>i\in s.released
NoExecEndWithoutStart == \A i\in 1..2:s.terminal[i]\in{"completed","cancelled","failed"}=>i\in s.started
OldGenerationCannotChoose == [][\A g\in 1..2,k\in{"completed","cancelled","failed"}:Choose(g,k)=>g=s.runGeneration[s.active]]_s
TerminalNeverReplaced == [][\A i\in 1..2:s.terminal[i]#"none"=>s.terminal'[i]=s.terminal[i]]_s
SkippedQueueDoesNotStart == [][Dequeue /\ (Head(s.queued)\in s.cancelled \/ s.closed)=>s.started'=s.started /\ s.active'=0]_s
=============================================================================
