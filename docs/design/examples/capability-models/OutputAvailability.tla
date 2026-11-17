-------------------- MODULE OutputAvailability --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[started|->FALSE,ended|->FALSE,live|->FALSE,file|->FALSE,closed|->FALSE,reply|->"none"]
Start == /\ ~s.started /\ ~s.closed /\ s'=[s EXCEPT !.started=TRUE, !.live=TRUE, !.file=TRUE]
Finish == /\ s.started /\ s'=[s EXCEPT !.ended=TRUE]
Reload == s'=[s EXCEPT !.live=FALSE, !.closed=FALSE]
Close == s'=[s EXCEPT !.closed=TRUE]
LoseFile == s'=[s EXCEPT !.file=FALSE]
Preview == s'=[s EXCEPT !.reply=IF s.closed THEN "closed" ELSE IF ~s.live THEN "unknown" ELSE "preview"]
Page(limit,queryOk,ioOk) == s'=[s EXCEPT !.reply=CASE limit=0->"invalidLimit" [] s.closed->"closed"
 [] ~s.live /\ ~queryOk->"readError" [] ~s.started->"unknown"
 [] ~s.file \/ ~ioOk->"unavailable" [] OTHER->"page"]
Next == Start\/Finish\/Reload\/Close\/LoseFile\/Preview\/(\E l\in 0..2,q,i\in BOOLEAN:Page(l,q,i))
Spec == Init /\ [][Next]_s
NoLiveWithoutStart == s.live=>s.started
ReadsPreserveTerminal == [][Preview\/(\E l\in 0..2,q,i\in BOOLEAN:Page(l,q,i))=>s.ended'=s.ended /\ s.started'=s.started]_s
PreviewNeedsLiveCapture == [][Preview /\ ~s.closed /\ ~s.live=>s.reply'="unknown"]_s
PageDoesNotUsePreview == [][\A l\in 1..2:Page(l,TRUE,TRUE) /\ ~s.closed /\ s.started /\ ~s.file=>s.reply'="unavailable"]_s
HistoricalFileStillReadable == [][\A l\in 1..2:Page(l,TRUE,TRUE) /\ ~s.closed /\ s.started /\ s.file=>s.reply'="page"]_s
=============================================================================
