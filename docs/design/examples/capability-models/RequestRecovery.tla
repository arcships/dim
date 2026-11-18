-------------------- MODULE RequestRecovery --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
CONSTANT AllowFailure
VARIABLE s
Owners == {"loop", "prepare", "model"}
Fact(t,r,k,w) == [turn|->t,request|->r,kind|->k,writer|->w]
Current(k,w) == Fact(2,2,k,w)
Kinds == <<"start","prepare","call","end">>
CurrentPrefix(n) == [i \in 1..n |-> Current(Kinds[i],"old")]
OldPrefix(t,r) == [i \in 1..4 |-> Fact(t,r,Kinds[i],"old")]
Matches(e) == e.turn=2 /\ e.request=2
Has(log,k) == \E i \in 1..Len(log): Matches(log[i]) /\ log[i].kind=k
LoopSelect(log) == Has(log,"start") /\ ~Has(log,"prepare") /\ ~Has(log,"end")
PrepareSelect(log) == Has(log,"prepare") /\ ~Has(log,"call") /\ ~Has(log,"end")
ModelSelect(log) == Has(log,"call") /\ ~Has(log,"end")
Select(o,log) == CASE o="loop" -> LoopSelect(log)
 [] o="prepare" -> PrepareSelect(log) [] o="model" -> ModelSelect(log)
Responsible(n) == CASE n=1 -> "loop" [] n=2 -> "prepare" [] n=3 -> "model" [] OTHER -> "old"
Init == \E n \in 1..4: \E identity \in {<<1,2>>,<<2,1>>}, p \in 0..(4+n):
 s=[log|->OldPrefix(identity[1],identity[2]) \o CurrentPrefix(n),
 initialStage|->n, published|->p, phase|->[o \in Owners |-> "new"],
 cut|->[o \in Owners |-> 0], selected|->[o \in Owners |-> FALSE],fault|->FALSE]
Capture(o) == /\ s.phase[o]="new"
 /\ s'=[s EXCEPT !.phase[o]="captured", !.cut[o]=Len(s.log), !.selected[o]=Select(o,s.log)]
QueryFail(o) == /\ AllowFailure /\ s.phase[o]="new"
 /\ s'=[s EXCEPT !.phase[o]="failed"]
Complete(o) == /\ s.phase[o]="captured"
 /\ (~s.selected[o] \/ ~s.fault)
 /\ s'=[s EXCEPT !.phase[o]="done",
 !.log=IF s.selected[o] THEN Append(@,Current("end",o)) ELSE @]
WriteFail(o) == /\ s.phase[o]="captured" /\ s.selected[o] /\ (AllowFailure \/ s.fault)
 /\ s'=[s EXCEPT !.phase[o]="failed", !.fault=TRUE]
Publish == /\ ~s.fault /\ s.published<Len(s.log)
 /\ s'=[s EXCEPT !.published=@+1]
Next == Publish \/ (\E o \in Owners: Capture(o) \/ QueryFail(o) \/ Complete(o) \/ WriteFail(o))
Spec == Init /\ [][Next]_s
Terminals == {i \in 1..Len(s.log): Matches(s.log[i]) /\ s.log[i].kind="end"}
AtMostOneTerminal == Cardinality(Terminals)<=1
CorrectSettlementOwner == \A i \in Terminals: s.log[i].writer=Responsible(s.initialStage)
AllSuccessfulMeansSettled == (\A o \in Owners: s.phase[o]="done") => Cardinality(Terminals)=1
PublicationBound == s.published<=Len(s.log)
=============================================================================
