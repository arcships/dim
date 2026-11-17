-------------------- MODULE PromptAdoption --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[loaded |-> {}, requested |-> {}, candidate |-> 0, adopted |-> 0,
 history |-> <<>>, published |-> 0, closed |-> FALSE, replies |-> {}]
Request(i) == /\ ~s.closed /\ i\notin s.requested /\ s'=[s EXCEPT !.requested=@\cup{i}]
Load(i) == /\ i\in s.requested /\ i\notin s.loaded /\ s'=[s EXCEPT !.loaded=@\cup{i}]
Select(i) == /\ ~s.closed /\ i\in s.loaded /\ i>s.candidate /\ i>s.adopted
 /\ s'=[s EXCEPT !.candidate=i]
Commit == /\ ~s.closed /\ s.candidate>s.adopted
 /\ s'=[s EXCEPT !.history=Append(Append(Append(@,[kind|->"prompt",id|->s.candidate]),[kind|->"input",id|->s.candidate]),[kind|->"configured",id|->s.candidate]), !.adopted=s.candidate]
Publish == /\ s.published<Len(s.history) /\ s'=[s EXCEPT !.published=@+1]
DropReply(i) == s'=[s EXCEPT !.replies=@\cup{i}]
Close == s'=[s EXCEPT !.closed=TRUE]
Restore == /\ s.closed /\ s'=[s EXCEPT !.closed=FALSE, !.candidate=s.adopted, !.loaded={}, !.requested={}]
Next == (\E i\in 1..3:Request(i)\/Load(i)\/Select(i)\/DropReply(i))\/Commit\/Publish\/Close\/Restore
Spec == Init /\ [][Next]_s
MonotonicAdoption == [][s.adopted'>=s.adopted]_s
BatchComplete == Len(s.history)%3=0
DecisionLast == \A i\in 1..Len(s.history):s.history[i].kind="configured" =>
 i>=3 /\ s.history[i-1].kind="input" /\ s.history[i-2].kind="prompt" /\ s.history[i-1].id=s.history[i].id
LoadIsNotAdoption == [][s.history'=s.history => s.adopted'=s.adopted]_s
=============================================================================
