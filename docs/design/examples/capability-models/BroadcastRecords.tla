-------------------- MODULE BroadcastRecords --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[head|->[b\in 1..2|->0],cursor|->[b\in 1..2|->0],closed|->{},ended|->FALSE,records|-><<>>,notice|->FALSE,fence|->FALSE]
Commit(b) == /\ b\notin s.closed /\ s.head[b]<2 /\ s'=[s EXCEPT !.head[b]=@+1]
Event(b) == /\ ~s.ended /\ s.cursor[b]<s.head[b]
 /\ s'=[s EXCEPT !.cursor[b]=@+1, !.records=Append(@,[kind|->"event",branch|->b,id|-><<b,s.cursor[b]+1>>])]
Close(b) == /\ s.head[b]=s.cursor[b] /\ s'=[s EXCEPT !.closed=@\cup{b}]
Closed == /\ s.closed={1,2} /\ ~s.ended
 /\ s'=[s EXCEPT !.ended=TRUE, !.records=Append(@,[kind|->"closed",branch|->0,id|-><<0,0>>])]
Notice == /\ ~s.notice /\ s'=[s EXCEPT !.notice=TRUE, !.records=Append(@,[kind|->"notice",branch|->0,id|-><<0,1>>])]
Fence == /\ ~s.fence /\ s'=[s EXCEPT !.fence=TRUE, !.records=Append(@,[kind|->"fence",branch|->0,id|-><<0,2>>])]
Next == (\E b\in 1..2:Commit(b)\/Event(b)\/Close(b))\/Closed\/Notice\/Fence
Spec == Init /\ [][Next]_s
IdentityPreserved == \A n\in 1..Len(s.records):s.records[n].kind="event"=>s.records[n].id[1]=s.records[n].branch /\ s.records[n].id[2]<=s.head[s.records[n].branch]
NoBranchEventAfterClosed == \A i,j\in 1..Len(s.records):i<j /\ s.records[i].kind="closed"=>s.records[j].kind#"event"
PerBranchOrder == \A i,j\in 1..Len(s.records):i<j /\ s.records[i].kind="event" /\ s.records[j].kind="event" /\ s.records[i].branch=s.records[j].branch=>s.records[i].id[2]<s.records[j].id[2]
=============================================================================
