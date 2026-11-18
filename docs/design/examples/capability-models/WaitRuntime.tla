-------------------- MODULE WaitRuntime --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
\* Queue items are input ids: 1 and 2 carry task notices, 3 carries a steer.
\* A notice's kind is its own id and is also the task it reports; a steer's
\* kind is 0 and matches any target.
SteerInput == 3
KindOf(i) == IF i=SteerInput THEN 0 ELSE i
Remove(q,n) == SubSeq(q,1,n-1) \o SubSeq(q,n+1,Len(q))
Queue == {s.queue[n]:n\in 1..Len(s.queue)}
Matching(q,t) == SelectSeq(q,LAMBDA n:KindOf(n)=0 \/ t=0 \/ KindOf(n)=t)
Pick(q,t) == IF 0\in {KindOf(q[i]):i\in 1..Len(q)}
 THEN Head(SelectSeq(q,LAMBDA n:KindOf(n)=0))
 ELSE Head(Matching(q,t))
Init == s=[open|->FALSE, arrivals|->{}, adopted|->{}, seen|->{}, taken|->{}, queue|-><<>>, steerSeen|->FALSE,
 waiting|->0, closing|->FALSE, tracked|->{}, finished|->{}, generation|->0, accepted|->0,
 boundaries|-><<>>, recovered|->FALSE, target|->0, selections|-><<>>, outcomes|-><<>>,
 phase|->"idle", callerCancelled|->FALSE, expired|->FALSE, wake|->FALSE, rejected|->"none",
 notified|->{}]
Start == /\ ~s.closing /\ Len(s.boundaries)<2
 /\ s'=[s EXCEPT !.open=TRUE, !.boundaries=Append(@,TRUE), !.recovered=FALSE]
End == /\ ~s.closing /\ Len(s.boundaries)<2
 /\ s'=[s EXCEPT !.open=FALSE, !.arrivals={}, !.adopted={}, !.taken={}, !.queue= <<>>,
 !.notified={},
 !.boundaries=Append(@,FALSE), !.recovered=FALSE]
\* A released input: from here it can end a wait, until it is read or deleted.
Arrive(i) == /\ s.open /\ ~s.closing /\ i\notin s.seen
 /\ s'=[s EXCEPT !.arrivals=@\cup{i}, !.seen=@\cup{i}, !.queue=Append(@,i),
 !.wake=@\/(s.phase\in{"registered","sleep"}), !.recovered=FALSE]
Steer == /\ s.open /\ ~s.closing /\ ~s.steerSeen
 /\ s'=[s EXCEPT !.arrivals=@\cup{SteerInput}, !.queue=Append(@,SteerInput), !.steerSeen=TRUE,
 !.wake=@\/(s.phase\in{"registered","sleep"}), !.recovered=FALSE]
\* The model read the released input: it is retired and can never end a wait
\* again. A notice a wait already took is retired here too, and reading it is
\* what records its task for the rest of the turn.
Consume(i) == /\ s.open /\ ~s.closing /\ i\in s.arrivals
 /\ s'=[s EXCEPT !.queue=SelectSeq(@,LAMBDA n:n#i), !.arrivals=@\{i}, !.taken=@\{i},
   !.adopted=@\cup{i}, !.notified=@\cup({KindOf(i)}\{0})]
\* The input was deleted instead of read: it is retired the same way, and its
\* task is not recorded, so a later wait naming it still has to wait.
Delete(i) == /\ s.open /\ ~s.closing /\ i\in s.arrivals
 /\ s'=[s EXCEPT !.queue=SelectSeq(@,LAMBDA n:n#i), !.arrivals=@\{i}, !.taken=@\{i},
   !.adopted=@\cup{i}]
Admit(g,t,c) == /\ g=s.generation /\ s.accepted<2 /\ s.waiting=0 /\ ~s.closing
 /\ s'=[s EXCEPT !.waiting=s.accepted+1, !.tracked=@\cup{s.accepted+1}, !.accepted=@+1,
 !.target=t, !.phase="initial", !.callerCancelled=c, !.expired=FALSE, !.wake=FALSE]
Reject(g) == /\ g#s.generation \/ s.closing \/ s.waiting#0
 /\ s'=[s EXCEPT !.rejected=IF g#s.generation \/ s.closing THEN "reloading" ELSE "alreadyWaiting"]
Initial == /\ s.waiting#0 /\ s.phase="initial" /\ ~s.callerCancelled
 /\ s'=[s EXCEPT !.phase="poll"]
\* Registering admits the call to a wait. A call naming a task the model
\* already read is answered at this poll instead of registering, but only when
\* nothing released matches: a queued steer or another notice for that task is
\* taken first, so the call ends as steer or notification.
Register == /\ s.waiting#0 /\ s.phase="poll"
 /\ ~(s.target#0 /\ s.target\in s.notified /\ Matching(s.queue,s.target)= <<>>)
 /\ s'=[s EXCEPT !.phase="registered", !.wake=FALSE]
\* A released input leaves the queue when taken; it stays registered, so the
\* model reading it afterwards still records what it was.
Take(i) == /\ s.waiting#0 /\ s.phase="registered" /\ ~s.closing
 /\ Matching(s.queue,s.target)# <<>> /\ i=Pick(s.queue,s.target)
 /\ LET n==CHOOSE n\in 1..Len(s.queue):s.queue[n]=i IN
 s'=[s EXCEPT !.queue=Remove(@,n), !.taken=@\cup{i},
 !.selections=IF KindOf(i)=0 THEN @
   ELSE Append(@,[target|->s.target,task|->KindOf(i),input|->i]),
 !.outcomes=Append(@,[call|->s.waiting,reason|->IF KindOf(i)=0 THEN "steer" ELSE "notification",
   task|->KindOf(i),input|->i]),
 !.waiting=0, !.phase="idle", !.finished=@\cup{s.waiting}]
Sleep == /\ s.phase="registered" /\ ~s.closing /\ Matching(s.queue,s.target)= <<>>
 /\ s'=[s EXCEPT !.phase="sleep"]
\* The model already read the named task's notice and nothing released matches:
\* the call answers at once instead of waiting for a notice that will not come
\* again. It consumes no arrival and leaves the record in place, so a later call
\* for the same task answers the same way. The check is the poll before
\* registering, which the call repeats after each wake, so a record made while
\* it sleeps does not end the sleep.
AlreadyNotified == /\ s.waiting#0 /\ s.phase="poll" /\ ~s.closing
 /\ s.target#0 /\ s.target\in s.notified /\ Matching(s.queue,s.target)= <<>>
 /\ s'=[s EXCEPT !.waiting=0, !.phase="idle", !.finished=@\cup{s.waiting},
   !.outcomes=Append(@,[call|->s.waiting,reason|->"already_notified",task|->s.target,input|->0])]
Wake == /\ s.phase="sleep" /\ s.wake /\ s'=[s EXCEPT !.phase="poll"]
Cancel == /\ s.waiting#0 /\ s'=[s EXCEPT !.callerCancelled=TRUE]
Expire == /\ s.waiting#0 /\ s'=[s EXCEPT !.expired=TRUE]
CanEnd(r) == CASE r="drop" -> TRUE
 [] r="reloading" -> s.closing /\ ~(s.phase="initial" /\ s.callerCancelled)
 [] r="interrupted" -> s.callerCancelled /\ (s.phase="initial" \/ (s.phase="sleep" /\ ~s.closing))
 [] OTHER -> r="timeout" /\ s.phase="sleep" /\ s.expired /\ ~s.closing
EndCall(r) == /\ s.waiting#0 /\ CanEnd(r)
 /\ s'=[s EXCEPT !.waiting=0, !.phase="idle", !.finished=@\cup{s.waiting},
 !.outcomes=Append(@,[call|->s.waiting,reason|->r,task|->0,input|->0])]
ReleaseTracking(i) == /\ i\in s.tracked\cap s.finished /\ s'=[s EXCEPT !.tracked=@\{i}]
Close == /\ ~s.closing /\ s'=[s EXCEPT !.closing=TRUE]
Restore == /\ s.closing /\ s.tracked={} /\ s.generation<1
 /\ s'=[s EXCEPT !.closing=FALSE, !.generation=@+1, !.arrivals={}, !.adopted={}, !.taken={},
 !.queue= <<>>, !.notified={}, !.recovered=TRUE,
 !.open=IF Len(s.boundaries)=0 THEN FALSE ELSE s.boundaries[Len(s.boundaries)]]
Next == Start\/End\/(\E i\in 1..2:Arrive(i))\/Steer\/(\E i\in 1..3:Take(i))\/AlreadyNotified
 \/(\E g\in 0..1,t\in 0..2,c\in BOOLEAN:Admit(g,t,c))\/(\E g\in 0..1:Reject(g))
 \/Initial\/Register\/Sleep\/Wake\/Cancel\/Expire
 \/(\E i\in 1..3:Consume(i))\/(\E i\in 1..3:Delete(i))
 \/(\E r\in{"drop","reloading","interrupted","timeout"}:EndCall(r))
 \/(\E i\in 1..2:ReleaseTracking(i))\/Close\/Restore
ShutdownCall == s.closing /\ (EndCall("reloading")\/EndCall("interrupted"))
Spec == Init /\ [][Next]_s /\ WF_s(ShutdownCall) /\ (\A i\in 1..2:WF_s(ReleaseTracking(i)))
SlotTracked == s.waiting#0 => s.waiting\in s.tracked
PendingRegistered == Queue\subseteq s.arrivals
TakenRegistered == s.taken\subseteq s.arrivals
AdoptedNotPending == s.adopted\cap Queue={}
AdoptedNotRegistered == s.adopted\cap s.arrivals={}
TakenNotPending == s.taken\cap Queue={}
ClosedTurnEmpty == ~s.open => s.queue= <<>>
RestoredNoNotifications == s.recovered => s.queue= <<>>
SelectionMatches == \A i\in 1..Len(s.selections):s.selections[i].target\in{0,s.selections[i].task}
QueueMatchesSet == s.arrivals=Queue\cup s.taken
OneOutcomePerCall == Len(s.outcomes)=Cardinality({s.outcomes[i].call:i\in 1..Len(s.outcomes)})
NoLostArrival == s.phase="sleep" /\ Matching(s.queue,s.target)# <<>> => s.wake
\* Only reading a notice records its task, and reading needs an open turn whose
\* End clears the record, so the record never outlives the turn that made it.
NotifiedInTurn == s.notified# {} => s.open
NoAdmissionAfterClose == [][s.closing /\ s.generation'=s.generation => s.accepted'=s.accepted]_s
DrainBeforeRestore == [][s.generation'>s.generation => s.tracked={} /\ s.waiting=0]_s
OldHandleRejected == [][\A g\in 0..1,t\in 0..2,c\in BOOLEAN:g<s.generation=>~Admit(g,t,c)]_s
CloseDrains == s.closing ~> s.tracked={}
SteerPriority == [][\A i\in 1..2:Take(i)=>0\notin{KindOf(s.queue[n]):n\in 1..Len(s.queue)}]_s
RejectPreservesCall == [][\A g\in 0..1:Reject(g)=>s.tracked'=s.tracked /\ s.queue'=s.queue /\ s.waiting'=s.waiting]_s
PendingSurvivesOtherEnd == [][\A r\in{"drop","reloading","interrupted","timeout"}:EndCall(r)=>s.queue'=s.queue]_s
\* Reading a notice records its task; deleting one records nothing, so a
\* deleted notice never makes its task answered as already notified.
ReadRecordsTask ==
 [][\A i\in 1..3:Consume(i) => s.notified'=s.notified\cup({KindOf(i)}\{0})]_s
DeleteRecordsNothing == [][\A i\in 1..3:Delete(i) => s.notified'=s.notified]_s
EndClearsNotified == [][End => s.notified'={}]_s
RestoreClearsNotified == [][s.generation'>s.generation => s.notified'={}]_s
\* A call answers already_notified only for a task the record holds.
AlreadyNotifiedOnlyRead ==
 [][Len(s.outcomes')>Len(s.outcomes)
   /\ s.outcomes'[Len(s.outcomes')].reason="already_notified"
   => s.outcomes'[Len(s.outcomes')].task\in s.notified']_s
=============================================================================
