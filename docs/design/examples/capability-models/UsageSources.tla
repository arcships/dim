-------------------- MODULE UsageSources --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Sources == {"configured","consumed","outputEnd","toolResult","compaction","usage"}
Render == {"consumed","outputEnd","toolResult","compaction"}
Init == s=[events|-><<>>,counted|->{},delivered|->{},basis|->{},measurements|->{},calibrated|->{},initialized|->FALSE, predecessor|->{},query|->"none",cut|->0]
AppendEvent(kind,child) == /\ Len(s.events)<2 /\ LET i==Len(s.events)+1 render==kind\in Render /\ ~child eligible==kind\in Sources /\ (render \/ kind\in{"configured","usage"}) IN s'=[s EXCEPT !.events=Append(@,[kind|->kind,child|->child,render|->render]), !.counted=IF eligible THEN @\cup{i} ELSE @]
Deliver(i) == /\ i\in 1..Len(s.events) /\ s'=[s EXCEPT !.delivered=@\cup{i}, !.measurements=IF s.events[i].kind="usage" /\ i\in s.basis THEN @\cup{i} ELSE @]
Associate(i) == /\ i\in 1..Len(s.events) /\ i\notin s.delivered /\ s.events[i].kind="usage" /\ s'=[s EXCEPT !.basis=@\cup{i}]
Calibrate(i) == /\ i\in s.delivered\cap s.measurements /\ s'=[s EXCEPT !.calibrated=@\cup{i}]
Initialize == s'=[s EXCEPT !.initialized=TRUE]
SavePrefix(c) == s'=[s EXCEPT !.predecessor=@\cup{c}]
ReadPrefix(c) == s'=[s EXCEPT !.cut=c, !.query=IF c\in s.predecessor THEN "value" ELSE IF s.initialized /\ c>=1 THEN "zero" ELSE "unavailable"]
Next == (\E k\in Sources\cup{"other"},c\in BOOLEAN:AppendEvent(k,c)) \/(\E i\in 1..2:Deliver(i)\/Associate(i)\/Calibrate(i))\/Initialize\/(\E c\in 0..2:SavePrefix(c)\/ReadPrefix(c))
Spec == Init /\ [][Next]_s
ChildRenderDoesNotCount == \A i\in s.counted:s.events[i].kind\in{"configured","usage"} \/ ~s.events[i].child
OtherSourcesExcluded == \A i\in s.counted:s.events[i].kind\in Sources
CalibrationWaitsForDeliveryAndBasis == s.calibrated\subseteq s.delivered\cap s.basis
MissingHistoricalPrefixUnavailable == [][\A c\in 0..2:ReadPrefix(c) /\ c\notin s.predecessor /\ (~s.initialized \/ c<1)=>s.query'="unavailable"]_s
=============================================================================
