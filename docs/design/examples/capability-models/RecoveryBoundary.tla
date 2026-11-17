-------------------- MODULE RecoveryBoundary --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
RECURSIVE Fold(_)
Fold(q) == IF Len(q)=0 THEN FALSE ELSE IF q[Len(q)]="notice" THEN Fold(SubSeq(q,1,Len(q)-1)) ELSE q[Len(q)]="start"
Latest(h,c) == LET b=={i\in 1..c:h[i]#"notice"} IN
 IF b={} THEN FALSE ELSE h[CHOOSE i\in b:\A j\in b:i>=j]="start"
Init == s=[history |-> <<>>, cut |-> 0, cursor |-> 0, phase |-> "live", open |-> FALSE]
Source(k) == /\ Len(s.history)<5 /\ s'=[s EXCEPT !.history=Append(@,k)]
Capture == /\ s.phase="live" /\ s'=[s EXCEPT !.cut=Len(s.history), !.phase="query"]
Restore == /\ s.phase="query"
 /\ s'=[s EXCEPT !.open=Latest(s.history,s.cut), !.cursor=s.cut, !.phase="consume"]
Consume == /\ s.phase="consume" /\ s.cursor<Len(s.history)
 /\ LET n==s.cursor+1 IN s'=[s EXCEPT !.cursor=n,
 !.open=IF s.history[n]="notice" THEN @ ELSE s.history[n]="start"]
Next == (\E k\in{"start","end","notice"}:Source(k))\/Capture\/Restore\/Consume
Spec == Init /\ [][Next]_s /\ WF_s(Restore)
LatestEqualsFold == Latest(s.history,s.cut)=Fold(SubSeq(s.history,1,s.cut))
LivePrefixMatches == s.phase="consume"=>s.open=Fold(SubSeq(s.history,1,s.cursor))
NoSkippedSuffix == s.phase="consume"=>s.cut<=s.cursor /\ s.cursor<=Len(s.history)
RestoreFinishes == s.phase="query" ~> s.phase="consume"
=============================================================================
