-------------------- MODULE ContextUsage --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
RECURSIVE Sum(_)
Sum(q) == IF Len(q)=0 THEN 0 ELSE Head(q)+Sum(Tail(q))
Init == s=[history |-> <<>>, cursor |-> 0, total |-> 0, savedCut |-> 0, savedTotal |-> 0,
 covered |-> 0, saved |-> FALSE, valid |-> FALSE, live |-> TRUE]
Source(v) == /\ Len(s.history)<3 /\ s'=[s EXCEPT !.history=Append(@,v)]
InitializeFromActualRequest == /\ s.live /\ ~s.valid
 /\ s'=[s EXCEPT !.total=Sum(s.history), !.cursor=Len(s.history), !.valid=TRUE,
 !.saved=TRUE, !.savedCut=Len(s.history), !.savedTotal=Sum(s.history), !.covered=Len(s.history)]
Process == /\ s.live /\ s.valid /\ s.cursor<Len(s.history)
 /\ LET n==s.cursor+1 IN s'=[s EXCEPT !.cursor=n, !.total=@+s.history[n],
 !.savedCut=IF s.history[n]=0 THEN @ ELSE n,
 !.savedTotal=IF s.history[n]=0 THEN @ ELSE s.total+s.history[n], !.saved=TRUE]
Cover == /\ s.live /\ s.valid /\ s.cursor=Len(s.history)
 /\ s'=[s EXCEPT !.covered=s.cursor]
Unload == /\ s.live /\ s'=[s EXCEPT !.live=FALSE, !.valid=FALSE]
Recover == /\ ~s.live
 /\ LET tail=={i\in (s.savedCut+1)..Len(s.history):s.history[i]#0}
        ok==s.saved /\ tail={}
 IN s'=[s EXCEPT !.live=TRUE, !.valid=ok, !.total=IF ok THEN s.savedTotal ELSE 0,
 !.cursor=IF ok THEN Len(s.history) ELSE 0]
Next == (\E v\in 0..2:Source(v))\/InitializeFromActualRequest\/Process\/Cover\/Unload\/Recover
Spec == Init /\ [][Next]_s
SavedPrefix == s.saved => s.savedTotal=Sum(SubSeq(s.history,1,s.savedCut))
CorrectLivePrefix == s.valid => s.total=Sum(SubSeq(s.history,1,s.cursor))
CoveredNoMissingContribution == s.covered>s.savedCut => \A i\in (s.savedCut+1)..s.covered:s.history[i]=0
=============================================================================
