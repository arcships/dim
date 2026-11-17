-------------------- MODULE CollaborationIdentity --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
CONSTANT OtherMember
Req == 1..2
Member(r) == IF r=2 THEN OtherMember ELSE 1
Branch(r) == IF r=2 THEN 2 ELSE 1
Init == s=[asked|->{},current|->[m\in 1..2|->0],inputs|->{},adopted|->{},idle|->{},derived|->[r\in Req|->"none"],delivered|->{},refused|->{},ended|->{},
 cancelled|->{},members|->{1,2},heldApproval|->{},withdrawn|->{},results|->[r\in Req|->"none"],restored|->FALSE]
Request(r) == /\ r\notin s.asked /\ ~s.restored
 /\ LET m==Member(r) busy==s.current[m]#0 \/ m\notin s.members IN
 s'=[s EXCEPT !.asked=@\cup{r}, !.current[m]=IF busy THEN @ ELSE r,
 !.refused=IF busy THEN @\cup{r} ELSE @, !.heldApproval=IF busy THEN @ ELSE @\cup{<<r,1>>,<<r,2>>}]
Input(r) == /\ r\in s.asked\s.refused /\ s.current[Member(r)]=r /\ ~s.restored
 /\ s'=[s EXCEPT !.inputs=@\cup{r}]
Adopt(r) == /\ r\in s.inputs /\ s'=[s EXCEPT !.adopted=@\cup{r}]
Deliver(r,ok) == /\ r\in s.asked\s.delivered /\ (r\in s.inputs\cup s.refused \/ ~ok)
 /\ s'=[s EXCEPT !.delivered=@\cup{r}, !.refused=IF ok THEN @ ELSE @\cup{r}]
Cancel(m,b) == /\ s.current[m]#0 /\ Branch(s.current[m])=b
 /\ s'=[s EXCEPT !.cancelled=@\cup{s.current[m]}]
Message(m) == UNCHANGED s
Close(m) == s'=[s EXCEPT !.members=@\{m}]
Idle(r,outcome) == /\ r\in s.delivered /\ r\notin s.refused\cup s.idle /\ s'=[s EXCEPT !.idle=@\cup{r}, !.derived[r]=outcome]
Settle(r) == /\ r\in s.asked\s.ended /\ (r\in s.idle\cup s.refused \/ Member(r)\notin s.members \/ s.restored)
 /\ s'=[s EXCEPT !.ended=@\cup{r}, !.current[Member(r)]=IF @=r THEN 0 ELSE @,
 !.results[r]=IF r\in s.refused \/ Member(r)\notin s.members \/ s.restored THEN "failed"
 ELSE s.derived[r]]
Withdraw(a) == /\ a\in s.heldApproval /\ a[1]\in s.ended /\ s'=[s EXCEPT !.withdrawn=@\cup{a}]
Restore == s'=[s EXCEPT !.restored=TRUE]
Next == (\E r\in Req:Request(r)\/Input(r)\/Adopt(r)\/Settle(r))\/(\E r\in Req,b\in BOOLEAN:Deliver(r,b))
 \/(\E r\in Req,o\in{"completed","failed","cancelled"}:Idle(r,o))
 \/(\E m,b\in 1..2:Cancel(m,b))\/(\E m\in 1..2:Message(m)\/Close(m))\/(\E a\in Req\X(1..2):Withdraw(a))\/Restore
Spec == Init /\ [][Next]_s /\ (\A a\in Req\X(1..2):WF_s(Withdraw(a)))
CurrentBelongsToMember == \A m\in 1..2:s.current[m]#0=>Member(s.current[m])=m /\ s.current[m]\notin s.ended
ApprovalBelongsToRequest == s.withdrawn\subseteq {a\in s.heldApproval:a[1]\in s.ended}
SettledResultStable == [][\A r\in s.ended:s.results'[r]=s.results[r]]_s
MessageDoesNotCancel == [][\A m\in 1..2:Message(m)=>s.cancelled'=s.cancelled /\ s.current'=s.current]_s
CancelNamesCurrentBranchRequest == [][\A m,b\in 1..2:Cancel(m,b)=>s.cancelled'=s.cancelled\cup{s.current[m]} /\ Branch(s.current[m])=b]_s
DeliveryFailureDoesNotUndoAdoption == [][\A r\in Req:Deliver(r,FALSE)=>s.adopted'=s.adopted]_s
EachApprovalRetires == \A a\in Req\X(1..2):(a\in s.heldApproval /\ a[1]\in s.ended)~>(a\in s.withdrawn)
CancellationCannotRewriteCompleted == [][\A r\in Req:Settle(r) /\ s.derived[r]="completed" /\ r\notin s.refused /\ Member(r)\in s.members /\ ~s.restored=>s.results'[r]="completed"]_s
=============================================================================
