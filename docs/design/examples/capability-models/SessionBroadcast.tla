-------------------- MODULE SessionBroadcast --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Branches == {1,2}
Init == s=[history |-> [b\in Branches|-><<>>], cursor |-> [b\in Branches|->0],
 notices |-> <<>>, closed |-> {}, sessionClosed |-> FALSE]
Commit(b) == /\ b\notin s.closed /\ Len(s.history[b])<2
 /\ s'=[s EXCEPT !.history[b]=Append(@,Len(@)+1)]
Deliver(b) == /\ ~s.sessionClosed /\ s.cursor[b]<Len(s.history[b])
 /\ s'=[s EXCEPT !.cursor[b]=@+1, !.notices=Append(@,[branch|->b,sequence|->s.cursor[b]+1])]
Close(b) == /\ s.cursor[b]=Len(s.history[b]) /\ s'=[s EXCEPT !.closed=@\cup{b}]
EndSession == /\ s.closed=Branches /\ s'=[s EXCEPT !.sessionClosed=TRUE]
Next == (\E b\in Branches:Commit(b)\/Deliver(b)\/Close(b))\/EndSession
Spec == Init /\ [][Next]_s
PublishedSourcesOnly == \A i\in 1..Len(s.notices):s.notices[i].sequence<=Len(s.history[s.notices[i].branch])
PerBranchOrder == \A i,j\in 1..Len(s.notices):i<j /\ s.notices[i].branch=s.notices[j].branch => s.notices[i].sequence<s.notices[j].sequence
CloseAfterBranchDrain == s.sessionClosed => \A b\in Branches:s.cursor[b]=Len(s.history[b])
NoPostCloseNotice == [][s.sessionClosed => s.notices'=s.notices]_s
=============================================================================
