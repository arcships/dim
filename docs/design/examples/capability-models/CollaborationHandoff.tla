-------------------- MODULE CollaborationHandoff --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Ids == 1..2
Init == s=[requests |-> {}, delivered |-> {}, inputs |-> {}, results |-> {},
 members |-> Ids, interrupted |-> {}, heldApproval |-> {}, withdrawn |-> {},
 restored |-> FALSE, closed |-> FALSE, turnEnds |-> 0]
Request(i) == /\ ~s.closed /\ ~s.restored /\ i\notin s.requests
 /\ s'=[s EXCEPT !.requests=@\cup{i}, !.heldApproval=@\cup{i}]
PublishInput(i) == /\ ~s.restored /\ ~s.closed /\ i\in s.requests\(s.delivered\cup s.inputs)
 /\ i\in s.members\s.interrupted
 /\ s'=[s EXCEPT !.inputs=@\cup{i}]
Deliver(i) == /\ ~s.restored /\ i\in s.requests\s.delivered /\ ~s.closed
 /\ (i\in s.inputs \/ i\notin s.members \/ i\in s.interrupted)
 /\ s'=[s EXCEPT !.delivered=@\cup{i}]
Result(i) == /\ i\in s.requests\s.results /\ i\in s.delivered
 /\ s'=[s EXCEPT !.results=@\cup{i}]
TurnEnd == /\ s.turnEnds<2 /\ s'=[s EXCEPT !.turnEnds=@+1]
MemberGone(i) == s'=[s EXCEPT !.members=@\{i}]
Cancel(i) == /\ i\in s.requests /\ s'=[s EXCEPT !.interrupted=@\cup{i}]
Close == s'=[s EXCEPT !.closed=TRUE]
Restore == /\ ~s.restored /\ s'=[s EXCEPT !.restored=TRUE]
Recover(i) == /\ s.restored /\ i\in s.requests
 /\ (s.closed \/ i\notin s.delivered \/ i\notin s.members \/ i\in s.interrupted)
 /\ s'=[s EXCEPT !.delivered=@\cup{i}, !.results=@\cup{i}]
WithdrawApproval(i) == /\ i\in s.heldApproval\s.withdrawn /\ i\in s.results
 /\ s'=[s EXCEPT !.withdrawn=@\cup{i}]
Next == (\E i\in Ids:Request(i)\/PublishInput(i)\/Deliver(i)\/Result(i)\/MemberGone(i)\/Cancel(i)\/Recover(i)\/WithdrawApproval(i))\/TurnEnd\/Close\/Restore
Spec == Init /\ [][Next]_s /\ (\A i\in Ids:WF_s(Recover(i))/\WF_s(WithdrawApproval(i)))
ResultsHaveRequests == s.results\subseteq s.requests
InputsHaveRequest == s.inputs\subseteq s.requests
NoRecoveryResend == [][s.restored=>s.inputs'=s.inputs]_s
WithdrawalHasSettlement == s.withdrawn\subseteq s.results
TurnIsNotRequestEnd == [][TurnEnd => s.results'=s.results]_s
MissingOwnerSettles == \A i\in Ids:(s.restored /\ i\in s.requests /\ i\notin s.members) ~> (i\in s.results)
ApprovalEventuallyWithdrawn == \A i\in Ids:(i\in s.heldApproval\cap s.results) ~> (i\in s.withdrawn)
=============================================================================
