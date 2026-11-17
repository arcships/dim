-------------------- MODULE InputReceipts --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[held|->{1,2},commands|-><<>>,decided|->0,owed|-><<>>,results|-><<>>,published|->0,
 replaced|->{},deleted|->{},receipt|->"none",observable|->2,snapshotAt|->0,closing|->FALSE]
Command(i,replace) == /\ Len(s.commands)<2 /\ ~s.closing
 /\ s'=[s EXCEPT !.commands=Append(@,[input|->i,replace|->replace])]
Decide == /\ s.decided<Len(s.commands)
 /\ LET c==s.commands[s.decided+1] accepted==c.input\in s.held IN
 s'=[s EXCEPT !.decided=@+1, !.held=@\{c.input},
 !.owed=Append(@,[command|->s.decided+1,input|->c.input,accepted|->accepted,replace|->accepted /\ c.replace])]
Commit == /\ Len(s.results)<Len(s.owed)
 /\ LET c==s.owed[Len(s.results)+1] IN s'=[s EXCEPT !.results=Append(@,c),
 !.deleted=IF c.accepted THEN @\cup{c.input} ELSE @, !.replaced=IF c.replace THEN @\cup{c.input} ELSE @]
Publish == /\ s.published<Len(s.results) /\ ~s.closing
 /\ s'=[s EXCEPT !.published=@+1]
Snapshot == /\ ~s.closing /\ s.published=Len(s.results) /\ Len(s.results)=s.decided
 /\ s'=[s EXCEPT !.observable=Cardinality(s.held), !.snapshotAt=s.published]
Reply(n) == /\ n\in 1..Len(s.commands)
 /\ s'=[s EXCEPT !.receipt=IF n<=s.published THEN IF s.results[n].accepted THEN "deleted" ELSE "notHeld" ELSE IF s.closing THEN "PublicationUnknown" ELSE "waiting"]
Close == s'=[s EXCEPT !.closing=TRUE]
Recover == s'=[s EXCEPT !.closing=FALSE]
Next == (\E i\in 1..2,b\in BOOLEAN:Command(i,b))\/Decide\/Commit\/Publish\/Snapshot\/(\E n\in 1..2:Reply(n))\/Close\/Recover
Spec == Init /\ [][Next]_s
ResultsInCommandOrder == \A n\in 1..Len(s.results):s.results[n].command=n
NotHeldNeverReplaces == \A n\in 1..Len(s.results):~s.results[n].accepted=>~s.results[n].replace
AtomicReplacement == s.replaced\subseteq s.deleted
OneDeletionPerInput == Cardinality(s.deleted)=Cardinality({n\in 1..Len(s.results):s.results[n].accepted})
SnapshotWaitsForResults == [][Snapshot=>s.published=s.decided]_s
CloseKeepsSnapshot == [][Close=>s.observable'=s.observable]_s
ConfirmedReplyHasPublication == [][\A n\in 1..2:Reply(n) /\ s.receipt'\in{"deleted","notHeld"}=>n<=s.published]_s
=============================================================================
