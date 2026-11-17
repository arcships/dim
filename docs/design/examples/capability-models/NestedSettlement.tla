-------------------- MODULE NestedSettlement --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Parents == 1..2
Children == 1..3
Ordered(S) == SelectSeq(<<1,2,3,4,5>>,LAMBDA i:i\in S)
Init == \E owner\in [Children->Parents]:s=[owner|->owner,accepted|->{},effects|->{},childDone|->{},
 parentDone|->{},parentEffects|->[p\in Parents|-><<>>],returned|->[p\in Parents|-><<>>],
 confirmedAtEnd|->[p\in Parents|->{}],waitingAtEnd|->[p\in Parents|->{}],
 deadlineAtEnd|->[p\in Parents|->FALSE],cancelled|->FALSE,expired|->FALSE]
Owned(p) == {i\in Children:s.owner[i]=p}
Confirmed(p) == s.effects\cap s.childDone\cap Owned(p)
Child(i) == /\ s.owner[i]\notin s.parentDone /\ ~s.cancelled /\ i\notin s.accepted
 /\ s'=[s EXCEPT !.accepted=@\cup{i}]
Effect(i) == /\ i\in s.accepted\s.childDone /\ s'=[s EXCEPT !.effects=@\cup{i}]
ChildResult(i) == /\ i\in s.accepted\s.childDone /\ s'=[s EXCEPT !.childDone=@\cup{i}]
Cancel == s'=[s EXCEPT !.cancelled=TRUE]
Expire == /\ s.cancelled /\ s'=[s EXCEPT !.expired=TRUE]
ParentResult(p,hasReturn) == /\ p\notin s.parentDone /\ (s.accepted\cap Owned(p)\subseteq s.childDone \/ s.expired)
 /\ LET returned==IF hasReturn THEN Ordered(Confirmed(p)\cup{p+3}) ELSE <<>> IN
 s'=[s EXCEPT !.parentDone=@\cup{p}, !.returned[p]=returned,
 !.parentEffects[p]=IF returned# <<>> THEN returned ELSE Ordered(Confirmed(p)),
 !.confirmedAtEnd[p]=Confirmed(p), !.waitingAtEnd[p]=(s.accepted\cap Owned(p))\s.childDone,
 !.deadlineAtEnd[p]=s.expired]
Next == (\E i\in Children:Child(i)\/Effect(i)\/ChildResult(i))\/Cancel\/Expire\/(\E p\in Parents,b\in BOOLEAN:ParentResult(p,b))
Spec == Init /\ [][Next]_s
ConfirmedEffectsRetained == \A p\in s.parentDone:s.confirmedAtEnd[p]\subseteq{s.parentEffects[p][n]:n\in 1..Len(s.parentEffects[p])}
OnlyOwnEffects == \A p\in Parents: \A n\in 1..Len(s.parentEffects[p]):s.parentEffects[p][n]\in(Owned(p)\cup{p+3})
NoDuplicateAugmentation == \A p\in Parents:Len(s.parentEffects[p])=Cardinality({s.parentEffects[p][n]:n\in 1..Len(s.parentEffects[p])})
ReturnedEffectsUnchanged == \A p\in s.parentDone:s.returned[p]# <<>>=>s.parentEffects[p]=s.returned[p]
ChildBeforeParentUnlessDeadline == \A p\in s.parentDone:~s.deadlineAtEnd[p]=>s.waitingAtEnd[p]={}
CancellationIsNotRollback == [][Cancel=>s.effects'=s.effects]_s
=============================================================================
