-------------------- MODULE TreeOpening --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Ids == 1..2
Init == s=[heads|->[p\in Ids|->1],parent|->[i\in Ids|->0],cut|->[i\in Ids|->0],accepted|->{},
 receipt|->{},input|->{},fenced|->{},published|->{},cancelled|->{},resources|->{},released|->{},failed|->{},
 closedParents|->{},droppedWaiters|->{},endRequested|->{},toolEnds|->{},turnEnded|->{},ended|->{}]
Advance(p) == /\ s.heads[p]<2 /\ s'=[s EXCEPT !.heads[p]=@+1]
Reserve(i,p) == /\ i\notin s.accepted /\ p\notin s.closedParents
 /\ s'=[s EXCEPT !.accepted=@\cup{i}, !.parent[i]=p, !.cut[i]=s.heads[p], !.resources=@\cup{i}]
Receipt(i,ok) == /\ i\in s.accepted\s.published /\ i\notin s.failed
 /\ s'=[s EXCEPT !.receipt=IF ok THEN @\cup{i} ELSE @, !.cancelled=IF ok THEN @ ELSE @\cup{i}]
InitialInput(i) == /\ i\in s.receipt /\ i\notin s.cancelled /\ s'=[s EXCEPT !.input=@\cup{i}]
Fence(i) == /\ i\in s.input /\ s'=[s EXCEPT !.fenced=@\cup{i}]
Publish(i) == /\ i\in s.accepted\s.cancelled /\ i\in s.receipt\cap s.input\cap s.fenced
 /\ s.parent[i]\notin s.closedParents /\ s'=[s EXCEPT !.published=@\cup{i}]
CancelBuild(i) == /\ i\in s.accepted\s.published /\ s'=[s EXCEPT !.cancelled=@\cup{i}]
Release(i) == /\ i\in s.cancelled /\ s'=[s EXCEPT !.resources=@\{i}, !.released=@\cup{i}]
Fail(i) == /\ i\in s.cancelled\cap s.released /\ i\notin s.published /\ s'=[s EXCEPT !.failed=@\cup{i}]
CloseParent(p) == s'=[s EXCEPT !.closedParents=@\cup{p}, !.cancelled=@\cup{i\in s.accepted\s.published:s.parent[i]=p}]
DropWaiter(i) == s'=[s EXCEPT !.droppedWaiters=@\cup{i}]
EndRequest(i,tool) == /\ i\in s.published\s.endRequested
 /\ s'=[s EXCEPT !.endRequested=@\cup{i}, !.toolEnds=IF tool THEN @\cup{i} ELSE @]
TurnEnd(i) == s'=[s EXCEPT !.turnEnded=@\cup{i}]
End(i) == /\ i\in s.endRequested /\ (i\notin s.toolEnds \/ i\in s.turnEnded)
 /\ s'=[s EXCEPT !.ended=@\cup{i}, !.resources=@\{i}]
Next == (\E i,p\in Ids:Reserve(i,p))\/(\E p\in Ids:Advance(p)\/CloseParent(p))
 \/(\E i\in Ids,b\in BOOLEAN:Receipt(i,b)\/EndRequest(i,b))
 \/(\E i\in Ids:InitialInput(i)\/Fence(i)\/Publish(i)\/CancelBuild(i)\/Release(i)\/Fail(i)\/DropWaiter(i)\/TurnEnd(i)\/End(i))
Spec == Init /\ [][Next]_s /\ (\A i\in Ids:WF_s(Release(i))/\WF_s(Fail(i)))
PublicationNeedsOpening == s.published\subseteq s.receipt\cap s.input\cap s.fenced\cap s.accepted
FailureSettlesResources == s.failed\cap s.resources={} /\ s.failed\cap s.published={}
ToolEndWaitsTurn == s.ended\cap s.toolEnds\subseteq s.turnEnded
StableAcceptedIdentity == [][\A i\in s.accepted:s.cut'[i]=s.cut[i] /\ s.parent'[i]=s.parent[i]]_s
WaiterDoesNotCancelAcceptedWork == [][\A i\in Ids:DropWaiter(i)=>s.cancelled'=s.cancelled /\ s.cut'=s.cut]_s
ParentCloseBlocksPublication == [][\A i\in Ids:Publish(i)=>s.parent[i]\notin s.closedParents]_s
CancelledBuildSettles == \A i\in Ids:(i\in s.cancelled /\ i\notin s.published)~>(i\in s.failed)
=============================================================================
