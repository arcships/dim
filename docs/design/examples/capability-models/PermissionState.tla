-------------------- MODULE PermissionState --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Policies == {"allow", "ask", "deny"}
Init == s = [history |-> <<>>, published |-> 0, consumed |-> 0, written |-> 0,
             policy |-> "ask", version |-> 0, ancestor |-> "ask", closed |-> FALSE,
             accepted |-> {}, replies |-> {}, recovered |-> FALSE]
Write(client, expected, value) ==
 /\ ~s.closed /\ Len(s.history) < 3 /\ client \notin s.accepted /\ expected = s.written
 /\ s' = [s EXCEPT !.history = Append(@, [client |-> client, expected |-> expected, policy |-> value]),
                   !.written = Len(s.history)+1, !.accepted = @ \cup {client}]
Publish == /\ s.published < Len(s.history) /\ s' = [s EXCEPT !.published = @+1]
Consume == /\ ~s.closed /\ s.consumed < s.published
 /\ LET n == s.consumed+1 IN
 s' = [s EXCEPT !.consumed = n, !.version = n, !.policy = s.history[n].policy,
                !.replies = @ \cup {s.history[n].client}, !.recovered = FALSE]
Close == /\ ~s.closed /\ s' = [s EXCEPT !.closed = TRUE]
Restore == /\ s.closed /\ s.published = Len(s.history)
 /\ s' = [s EXCEPT !.closed = FALSE, !.consumed = Len(s.history),
 !.version = Len(s.history), !.written = Len(s.history), !.recovered = TRUE,
 !.policy = IF Len(s.history)=0 THEN s.ancestor ELSE s.history[Len(s.history)].policy]
Ancestor(v) == /\ Len(s.history)=0 /\ s.closed /\ s' = [s EXCEPT !.ancestor=v]
Next == (\E c \in 1..3, e \in 0..3, v \in Policies : Write(c,e,v))
 \/ Publish \/ Consume \/ Close \/ Restore \/ (\E v \in Policies : Ancestor(v))
Spec == Init /\ [][Next]_s
Versions == s.version=s.consumed /\ s.consumed<=s.published /\ s.published<=s.written /\ s.written=Len(s.history)
NoLostUpdate == \A i,j \in 1..Len(s.history) : s.history[i].expected=s.history[j].expected => i=j
DispatchedPolicy == s.consumed>0 => s.policy=s.history[s.consumed].policy
ReplyAfterDispatch == s.replies \subseteq {s.history[i].client : i \in 1..s.consumed}
InheritedVersion == s.recovered /\ ~s.closed /\ Len(s.history)=0 => s.version=0 /\ s.policy=s.ancestor
=============================================================================
