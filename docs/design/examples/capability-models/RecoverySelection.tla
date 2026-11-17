-------------------- MODULE RecoverySelection --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Ids == 1..2
Init == s=[history |-> <<>>, cut |-> 0, cursor |-> 0, selected |-> {},
 phase |-> "live", folded |-> {}, referencePos |-> 0, pageAfter |-> 0, recovered |-> {}]
Write(kind,id) == /\ Len(s.history)<4
 /\ (kind="start" \/ \E j\in 1..Len(s.history):s.history[j]=[kind|->"start",id|->id])
 /\ (kind="end" \/ ~\E j\in 1..Len(s.history):s.history[j]=[kind|->"start",id|->id])
 /\ s'=[s EXCEPT !.history=Append(@,[kind|->kind,id|->id])]
Capture == /\ s.phase="live"
 /\ s'=[s EXCEPT !.cut=Len(s.history), !.phase="query"]
UnmatchedAt(c) == {i\in 1..c:s.history[i].kind="start" /\
 ~\E j\in 1..c:s.history[j].kind="end" /\ s.history[j].id=s.history[i].id}
Page == /\ s.phase="query" /\ s.pageAfter<s.cut
 /\ LET last==IF s.pageAfter+2<=s.cut THEN s.pageAfter+2 ELSE s.cut
        positions==UnmatchedAt(s.cut)\cap((s.pageAfter+1)..last)
 IN s'=[s EXCEPT !.pageAfter=last, !.selected=@\cup{s.history[i].id:i\in positions}]
Reference == /\ s.phase="query" /\ s.referencePos<s.cut
 /\ LET n==s.referencePos+1 e==s.history[n] IN
 s'=[s EXCEPT !.referencePos=n, !.folded=IF e.kind="start" THEN @\cup{e.id} ELSE @\{e.id}]
Finish == /\ s.phase="query" /\ s.pageAfter=s.cut /\ s.referencePos=s.cut
 /\ s'=[s EXCEPT !.phase="consume", !.cursor=s.cut, !.recovered=s.selected]
Consume == /\ s.phase="consume" /\ s.cursor<Len(s.history)
 /\ LET n==s.cursor+1 e==s.history[n] IN
 s'=[s EXCEPT !.cursor=n, !.recovered=IF e.kind="start" THEN @\cup{e.id} ELSE @\{e.id}]
Next == (\E k\in{"start","end"},i\in Ids:Write(k,i))\/Capture\/Page\/Reference\/Finish\/Consume
Spec == Init /\ [][Next]_s /\ WF_s(Page) /\ WF_s(Reference) /\ WF_s(Finish)
PagesKeepRelatedDomain == s.selected={s.history[i].id:i\in UnmatchedAt(s.cut)\cap(1..s.pageAfter)}
QueryEqualsReference == s.phase="consume" => s.selected=s.folded
RestoredAtCut == s.phase="consume" /\ s.cursor=s.cut => s.recovered=s.folded
CursorAfterCut == s.phase="consume" => s.cut<=s.cursor /\ s.cursor<=Len(s.history)
QueryFinishes == s.phase="query" ~> s.phase="consume"
=============================================================================
