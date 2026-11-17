-------------------- MODULE RequestCuts --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[history |-> <<>>, committed |-> 0, consumed |-> 0, requestCut |-> 0,
 controlCut |-> 0, preparing |-> FALSE, contents |-> {}, emitted |-> FALSE, captured |-> FALSE]
Source == /\ Len(s.history)<3 /\ s'=[s EXCEPT !.history=Append(@,Len(@)+1)]
Commit == /\ s.committed<Len(s.history) /\ s'=[s EXCEPT !.committed=@+1]
Consume == /\ s.consumed<s.committed /\ s'=[s EXCEPT !.consumed=@+1]
Prepare == /\ ~s.captured
 /\ s'=[s EXCEPT !.captured=TRUE, !.preparing=TRUE, !.requestCut=s.consumed, !.controlCut=s.consumed]
ReadContent == /\ s.preparing /\ s'=[s EXCEPT !.contents=1..s.requestCut]
ReadControl == /\ s.preparing /\ s'=[s EXCEPT !.controlCut=s.committed]
Emit == /\ s.preparing /\ s.contents=1..s.requestCut
 /\ s'=[s EXCEPT !.preparing=FALSE, !.emitted=TRUE]
Next == Source\/Commit\/Consume\/Prepare\/ReadContent\/ReadControl\/Emit
Spec == Init /\ [][Next]_s
NoFutureContent == s.contents\subseteq(1..s.requestCut)
ControlCanAdvance == s.captured=>s.controlCut>=s.requestCut
PublishedBoundary == s.consumed<=s.committed /\ s.committed<=Len(s.history)
CompleteContentCut == s.emitted=>s.contents=1..s.requestCut
=============================================================================
