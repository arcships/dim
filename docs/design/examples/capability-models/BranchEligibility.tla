-------------------- MODULE BranchEligibility --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == \E role\in{"main","tool","host"},notice\in BOOLEAN:s=[role|->role,notice|->notice,head|->1,opening|->1,cut|->0,cursor|->0,reported|->1,held|->FALSE,
 ended|->FALSE,turn|->FALSE,steered|->FALSE,ownEnd|->FALSE,pending|->FALSE,recovery|->FALSE,idle|->0,blocks|-><<>>,roster|->{1,2},endedNotice|->{},externalEnds|->{}]
ParentAdvance == /\ s.head<3 /\ s'=[s EXCEPT !.head=@+1]
Read(after,through,ok) == /\ after<=s.head /\ through<=s.head /\ through>=after
 /\ LET floor==IF s.role="main" THEN after ELSE IF after<s.cut THEN s.cut ELSE after
 IN /\ through>=floor /\ s'=[s EXCEPT !.cursor=IF ok THEN through ELSE @]
Block == /\ s.role#"main" /\ ~s.ended /\ ~s.held /\ s.head>s.opening /\ s.head>s.cursor /\ s.head>s.reported
 /\ s'=[s EXCEPT !.held=TRUE, !.reported=s.head, !.blocks=Append(@,[head|->s.head,cursor|->s.cursor,opening|->s.opening])]
ConsumeBlock == s'=[s EXCEPT !.held=FALSE]
Start == /\ ~s.turn /\ ~s.ended /\ ~s.recovery /\ s.idle<1
 /\ s'=[s EXCEPT !.turn=TRUE, !.steered=FALSE, !.ownEnd=FALSE]
Steer(ok) == /\ s.turn /\ s'=[s EXCEPT !.steered=@\/ok]
Pending(b) == s'=[s EXCEPT !.pending=b]
OwnEnd == /\ s.turn /\ s'=[s EXCEPT !.ownEnd=TRUE, !.ended=TRUE]
EndTurn == /\ s.turn /\ s.idle<1
 /\ s'=[s EXCEPT !.turn=FALSE, !.idle=IF s.role="tool" /\ s.notice /\ ~s.steered /\ ~s.ownEnd /\ ~s.pending THEN @+1 ELSE @]
Restore == /\ ~s.recovery /\ s'=[s EXCEPT !.recovery=TRUE, !.turn=FALSE]
ChildEnd(i,own) == /\ s.role="main" /\ i\in s.roster
 /\ s'=[s EXCEPT !.roster=@\{i}, !.endedNotice=IF own THEN @ ELSE @\cup{i}, !.externalEnds=IF own THEN @ ELSE @\cup{i}]
Next == ParentAdvance\/(\E a,t\in 0..3,b\in BOOLEAN:Read(a,t,b))\/Block\/ConsumeBlock\/Start
 \/(\E b\in BOOLEAN:Steer(b)\/Pending(b))\/OwnEnd\/EndTurn\/Restore\/(\E i\in 1..2,b\in BOOLEAN:ChildEnd(i,b))
Spec == Init /\ [][Next]_s
ParentBlockHasNewActivity == \A n\in 1..Len(s.blocks):s.blocks[n].head>s.blocks[n].cursor /\ s.blocks[n].head>s.blocks[n].opening
NoMainParentBlock == s.role="main"=>s.blocks= <<>>
EndNoticeAndRosterAtomic == s.endedNotice=s.externalEnds /\ s.endedNotice\cap s.roster={}
IdleEligibility == [][s.idle'>s.idle=>s.role="tool" /\ s.notice /\ ~s.steered /\ ~s.ownEnd /\ ~s.pending /\ s.turn]_s
RestoreNoOldIdle == [][Restore=>s.idle'=s.idle]_s
ExplicitReadCanReset == [][\A a,t\in 0..3:Read(a,t,TRUE)=>s.cursor'=t]_s
=============================================================================
