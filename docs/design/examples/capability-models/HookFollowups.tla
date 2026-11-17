-------------------- MODULE HookFollowups --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[chain|->1,round|->0,phase|->"turn",queued|->0,posted|-><<>>,normal|->FALSE,
 point|->"none",outcome|->"none",selected|->"none",reloaded|->FALSE]
End(kind,outcome) == /\ s.phase="turn"
 /\ s'=[s EXCEPT !.phase="hook", !.point=kind, !.outcome=outcome,
 !.selected=CASE kind="stop"->"Stop" [] kind="tool" /\ outcome="success"->"PostToolUse"
 [] kind="tool" /\ outcome="failure"->"PostToolUseFailure" [] OTHER->"none"]
Finish(blocks) == /\ s.phase="hook"
 /\ LET follow==s.selected="Stop" /\ blocks /\ s.round<3 IN
 s'=[s EXCEPT !.phase="idle", !.queued=IF follow THEN s.round+1 ELSE 0,
 !.posted=IF follow THEN Append(@,[chain|->s.chain,round|->s.round+1]) ELSE @]
ConsumeFollowup == /\ s.phase="idle" /\ s.queued>0
 /\ s'=[s EXCEPT !.round=s.queued, !.queued=0, !.phase="turn", !.reloaded=FALSE]
NormalTurn == /\ s.phase="idle" /\ s.queued=0 /\ s.chain=1
 /\ s'=[s EXCEPT !.chain=2, !.round=0, !.queued=0, !.phase="turn", !.normal=TRUE]
Reload == /\ ~s.reloaded /\ s.phase="idle"
 /\ s'=[s EXCEPT !.round=0, !.reloaded=TRUE]
Next == (\E k\in{"stop","tool"},o\in{"success","failure","recovered"}:End(k,o))
 \/(\E b\in BOOLEAN:Finish(b))\/ConsumeFollowup\/NormalTurn\/Reload
Spec == Init /\ [][Next]_s
BoundPerChain == \A c\in 1..2:Cardinality({i\in 1..Len(s.posted):s.posted[i].chain=c})<=3
RoundsAreOrdered == \A i,j\in 1..Len(s.posted):i<j /\ s.posted[i].chain=s.posted[j].chain=>s.posted[i].round<s.posted[j].round
PointMatchesOutcome == s.phase="hook" /\ s.point="tool"=>s.selected=CASE s.outcome="success"->"PostToolUse" [] s.outcome="failure"->"PostToolUseFailure" [] OTHER->"none"
OrdinaryTurnResets == [][NormalTurn=>s.round'=0]_s
NoContextOnlyFollowup == [][Finish(FALSE)=>s.posted'=s.posted]_s
RestoreKeepsQueuedRound == [][Reload=>s.queued'=s.queued]_s
=============================================================================
