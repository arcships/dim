-------------------- MODULE PromptOutcomes --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Keys == {<<1,1>>,<<1,2>>,<<2,1>>}
Ids == 1..2
Init == s=[asked|->{},targets|->[i\in Ids|->{}],values|->[i\in Ids|->0],
 owner|->[k\in Keys|->0],loaded|->[i\in Ids|->"pending"],result|->[i\in Ids|->"pending"],
 entries|->[k\in Keys|->0],closed|->FALSE,dropped|->{},committed|->{},validAtCommit|->{},
 before|->[i\in Ids|->[k\in Keys|->0]],after|->[i\in Ids|->[k\in Keys|->0]]]
Request(i,targets,value) == /\ ~s.closed /\ i\notin s.asked /\ targets#{}
 /\ s'=[s EXCEPT !.asked=@\cup{i}, !.targets[i]=targets, !.values[i]=value,
 !.owner=[k\in Keys|->IF k\in targets THEN i ELSE s.owner[k]]]
Load(i,ok) == /\ i\in s.asked /\ s.loaded[i]="pending"
 /\ s'=[s EXCEPT !.loaded[i]=IF ok THEN "ok" ELSE "error"]
Valid(i) == ~s.closed /\ \A k\in s.targets[i]:s.owner[k]=i
Adopt(i,writeOk) == /\ i\in s.asked /\ s.loaded[i]#"pending" /\ s.result[i]="pending"
 /\ LET valid==Valid(i)
        success==valid /\ s.loaded[i]="ok" /\ writeOk
        next==[k\in Keys|->IF k\in s.targets[i] THEN s.values[i] ELSE s.entries[k]]
 IN s'=[s EXCEPT !.result[i]=IF ~valid THEN "superseded" ELSE IF ~success THEN "error"
                          ELSE IF next=s.entries THEN "unchanged" ELSE "adopted",
 !.entries=IF success THEN next ELSE @,
 !.committed=IF success THEN @\cup{i} ELSE @, !.validAtCommit=IF success /\ valid THEN @\cup{i} ELSE @,
 !.before[i]=s.entries, !.after[i]=IF success THEN next ELSE s.entries]
Drop(i) == s'=[s EXCEPT !.dropped=@\cup{i}]
Close == s'=[s EXCEPT !.closed=TRUE]
Next == (\E i\in Ids,t\in SUBSET Keys,v\in 0..1:Request(i,t,v))
 \/(\E i\in Ids,b\in BOOLEAN:Load(i,b)\/Adopt(i,b))\/(\E i\in Ids:Drop(i))\/Close
Spec == Init /\ [][Next]_s
OnlyCurrentCandidateCommits == s.committed\subseteq s.validAtCommit
FullTargetIdentity == \A i\in Ids: \A k\in Keys\s.targets[i]:s.after[i][k]=s.before[i][k]
MissingEntryRevokes == \A i\in s.committed:s.values[i]=0=>\A k\in s.targets[i]:s.after[i][k]=0
FailureChangesNothing == \A i\in Ids:s.result[i]\in{"error","superseded"}=>s.after[i]=s.before[i]
SuccessHasMeaning == \A i\in Ids:(s.result[i]="adopted"=>s.after[i]#s.before[i]) /\ (s.result[i]="unchanged"=>s.after[i]=s.before[i])
DropDoesNotRevoke == [][\A i\in Ids:Drop(i)=>s.owner'=s.owner /\ s.entries'=s.entries /\ s.result'=s.result]_s
=============================================================================
