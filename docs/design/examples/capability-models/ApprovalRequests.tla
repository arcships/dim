-------------------- MODULE ApprovalRequests --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
CONSTANT OtherBranch
VARIABLE s
Keys == {<<1,1>>,<<OtherBranch,2>>}
Init == s=[pending|->Keys,closed|->{},cancelled|->{},denied|->{},commands|-><<>>,published|->0,consumed|->0,
 replies|-><<>>,answers|->[k\in Keys|->"waiting"],terminals|->{},auto|->{},autoPublished|->{},allowed|->{},failed|->{},lastQuery|->[code|->"none",keys|->{}]]
Respond(k,allow,ok) == /\ Len(s.commands)<2
 /\ IF k[1]\in s.closed THEN s'=[s EXCEPT !.lastQuery=[code|->"closed",keys|->{}]]
 ELSE IF k\notin s.pending THEN s'=[s EXCEPT !.lastQuery=[code|->"unknown",keys|->{}]]
 ELSE IF ~ok THEN s'=[s EXCEPT !.lastQuery=[code|->"closed",keys|->{}]]
 ELSE s'=[s EXCEPT !.commands=Append(@,[key|->k,allow|->allow]), !.terminals=@\cup{k}]
Publish == /\ s.published<Len(s.commands) /\ s'=[s EXCEPT !.published=@+1]
Consume == /\ s.consumed<s.published
 /\ LET c==s.commands[s.consumed+1] k==c.key
 result==IF k\notin s.pending THEN "unknown" ELSE IF k\in s.cancelled THEN "cancelled" ELSE "ok"
 IN s'=[s EXCEPT !.consumed=@+1, !.pending=@\{k}, !.replies=Append(@,[key|->k,result|->result]),
 !.answers[k]=IF result#"ok" THEN @ ELSE IF c.allow THEN "allow" ELSE "deny"]
Auto(k,ok) == /\ k\in s.pending /\ s.consumed=Len(s.commands)
 /\ s'=[s EXCEPT !.pending=@\{k}, !.auto=IF ok THEN @\cup{k} ELSE @,
 !.terminals=IF ok THEN @\cup{k} ELSE @, !.failed=IF ok THEN @ ELSE @\cup{k}]
PublishAuto(k) == /\ k\in s.auto /\ s'=[s EXCEPT !.autoPublished=@\cup{k}]
Return(k) == /\ (s.answers[k]="allow" \/ k\in s.autoPublished)
 /\ k\notin s.cancelled\cup s.denied\cup s.failed /\ k[1]\notin s.closed
 /\ s'=[s EXCEPT !.allowed=@\cup{k}]
Cancel(k,ok) == /\ k\notin s.allowed
 /\ s'=[s EXCEPT !.cancelled=@\cup{k}, !.terminals=IF ok /\ k\in s.pending THEN @\cup{k} ELSE @,
 !.pending=@\{k}]
MarkCancelled(k) == /\ k\notin s.allowed /\ s'=[s EXCEPT !.cancelled=@\cup{k}]
Deny(k) == s'=[s EXCEPT !.denied=@\cup{k}]
Close(b) == s'=[s EXCEPT !.closed=@\cup{b}, !.pending={k\in s.pending:k[1]#b}]
Query(b) == s'=[s EXCEPT !.lastQuery=[code|->"ok",keys|->{k\in s.pending:k[1]=b}]]
Next == (\E k\in Keys,b\in BOOLEAN:Auto(k,b)\/Cancel(k,b))\/(\E k\in Keys,a,o\in BOOLEAN:Respond(k,a,o))
 \/Publish\/Consume\/(\E k\in Keys:PublishAuto(k)\/Return(k)\/MarkCancelled(k)\/Deny(k))\/(\E b\in 1..2:Close(b)\/Query(b))
Spec == Init /\ [][Next]_s
AllowedHasPublishedDecision == \A k\in s.allowed:k\in s.terminals /\ (k\in s.autoPublished \/ s.answers[k]="allow")
FailedTerminalNeverAdmits == s.failed\cap s.allowed={}
OneResponsePerConsumedCommand == Len(s.replies)=s.consumed /\ \A n\in 1..Len(s.replies):s.replies[n].key=s.commands[n].key
ConsumeOnlyOwnRequest == [][Consume=>LET k==s.commands[s.consumed+1].key IN \A other\in Keys\{k}:s.answers'[other]=s.answers[other] /\ (other\in s.pending'<=>other\in s.pending)]_s
CancelOnlyOwnRequest == [][\A k\in Keys,b\in BOOLEAN:Cancel(k,b)=>\A other\in Keys\{k}:(other\in s.pending'<=>other\in s.pending)]_s
ReturnRechecksPolicy == [][\A k\in Keys:Return(k)=>k\notin s.denied\cup s.cancelled]_s
QueryIsBranchScoped == [][\A b\in 1..2:Query(b)=>s.lastQuery'.keys={k\in s.pending:k[1]=b}]_s
=============================================================================
