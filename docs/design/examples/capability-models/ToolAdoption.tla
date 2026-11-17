-------------------- MODULE ToolAdoption --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Ids == 1..2
Init == s=[registry|->1,watch|->FALSE,active|->FALSE,wake|->TRUE,loading|->0,adopted|->0,
 opened|->FALSE,closed|->FALSE,failedOpening|->FALSE,requests|->{},ended|->{},
 pins|->[i\in Ids|->0],declared|->[i\in Ids|->0],ran|->[i\in Ids|->0],commits|-><<>>]
Listen == s'=[s EXCEPT !.watch=TRUE]
Activate == s'=[s EXCEPT !.active=TRUE]
Change == /\ s.registry<3 /\ s'=[s EXCEPT !.registry=@+1, !.wake=@\/s.watch]
Read == /\ s.watch /\ s.active /\ ~s.closed /\ s.wake /\ s.loading=0
 /\ s'=[s EXCEPT !.loading=s.registry, !.wake=FALSE]
Commit == /\ s.loading#0 /\ ~s.closed
 /\ s'=[s EXCEPT !.adopted=s.loading, !.loading=0, !.opened=TRUE, !.commits=Append(@,s.loading)]
Fail == /\ s.loading#0 /\ ~s.closed
 /\ s'=[s EXCEPT !.loading=0, !.failedOpening=~s.opened, !.closed=~s.opened]
Prepare(i,empty) == /\ s.opened /\ ~s.closed /\ i\notin s.requests
 /\ s'=[s EXCEPT !.requests=@\cup{i}, !.pins[i]=IF empty THEN 0 ELSE s.adopted,
 !.declared[i]=IF empty THEN 0 ELSE s.adopted]
Run(i) == /\ i\in s.requests\s.ended /\ s.pins[i]#0
 /\ s'=[s EXCEPT !.ran[i]=s.pins[i]]
End(i) == /\ i\in s.requests /\ s'=[s EXCEPT !.ended=@\cup{i}, !.pins[i]=0]
Close == s'=[s EXCEPT !.closed=TRUE]
Next == Listen\/Activate\/Change\/Read\/Commit\/Fail\/Close
 \/(\E i\in Ids,b\in BOOLEAN:Prepare(i,b))\/(\E i\in Ids:Run(i)\/End(i))
Spec == Init /\ [][Next]_s
NoRequestBeforeOpening == s.requests#{}=>s.opened /\ s.active
PinsMatchDeclarations == \A i\in s.requests\s.ended:s.pins[i]=s.declared[i]
RunUsesOwnSnapshot == \A i\in Ids:s.ran[i]#0=>s.ran[i]=s.declared[i]
EndedReleasesOnlyOwnPin == \A i\in s.ended:s.pins[i]=0
NoHalfAdoption == s.adopted#0=>s.adopted=s.commits[Len(s.commits)]
FailedOpeningHasNoRequest == s.failedOpening=>s.requests={}
FailureRetainsAdoption == [][Fail=>s.adopted'=s.adopted /\ s.pins'=s.pins]_s
NoLostWatchChange == [][Change /\ s.watch=>s.wake']_s
NoCommitBeforeActivation == ~s.active=>s.commits= <<>>
=============================================================================
