-------------------- MODULE InputRecovery --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Ids == 1..2
Init == s=[held |-> {}, applied |-> {}, deleted |-> {}, requested |-> {}, history |-> <<>>,
 replacements |-> {}, closed |-> FALSE, recovered |-> {}, seen |-> {}, controls |-> [i\in Ids|->"none"]]
Input(i) == /\ ~s.closed /\ i\notin s.seen /\ s'=[s EXCEPT !.held=@\cup{i}, !.seen=@\cup{i}]
Apply(i) == /\ ~s.closed /\ i\in s.held /\ s.controls[i]="none"
 /\ s'=[s EXCEPT !.held=@\{i}, !.applied=@\cup{i}, !.controls[i]="apply"]
Withdraw(i) == /\ ~s.closed /\ i\in s.held /\ s.controls[i]="none"
 /\ s'=[s EXCEPT !.requested=@\cup{i}, !.controls[i]="withdraw"]
Replace(i) == /\ ~s.closed /\ i\in s.requested\s.deleted
 /\ s'=[s EXCEPT !.held=@\{i}, !.deleted=@\cup{i}, !.replacements=@\cup{i},
 !.history=Append(Append(@,[kind|->"replacement",id|->i]),[kind|->"delete",id|->i])]
Close == /\ ~s.closed /\ s'=[s EXCEPT !.closed=TRUE]
Restore == /\ s.closed
 /\ s'=[s EXCEPT !.closed=FALSE, !.recovered=s.seen\(s.applied\cup s.deleted), !.held=s.seen\(s.applied\cup s.deleted)]
Next == (\E i\in Ids:Input(i)\/Apply(i)\/Withdraw(i)\/Replace(i))\/Close\/Restore
Spec == Init /\ [][Next]_s
NoWithdrawAfterApply == s.requested\cap s.applied={}
AtomicReplacement == Len(s.history)=2*Cardinality(s.deleted)
DecisionLast == \A i\in 1..Len(s.history):s.history[i].kind="delete"=>i>1 /\ s.history[i-1]=[kind|->"replacement",id|->s.history[i].id]
NoRepeatReplacement == Cardinality(s.replacements)=Cardinality(s.deleted)
RestoreOnlyMembers == [][Restore=>s.held'\cap(s.deleted\cup s.applied)={}]_s
=============================================================================
