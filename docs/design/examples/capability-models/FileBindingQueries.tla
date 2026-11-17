-------------------- MODULE FileBindingQueries --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Keys == {<<1,1>>,<<1,2>>,<<2,1>>}
Init == s=[clock|->1,bindings|->[k\in Keys|->0],cut|->0,compactedAt|->0,
 queryKey|-><<1,1>>,queryBound|->0,queryCut|->0,queryCompaction|->0,published|->0,
 phase|->"idle",operation|->"write",allowed|->FALSE,generation|->0]
Bind(k) == /\ s.clock<4 /\ s.phase="idle"
 /\ s'=[s EXCEPT !.bindings[k]=s.clock, !.clock=@+1]
Compact(retain,ok) == /\ s.clock<4 /\ retain<=s.clock
 /\ s'=[s EXCEPT !.clock=@+1, !.cut=IF ok THEN retain ELSE @,
 !.compactedAt=IF ok THEN s.clock ELSE @]
Begin(k,op) == /\ s.phase="idle"
 /\ s'=[s EXCEPT !.phase="query", !.queryKey=k, !.queryBound=s.bindings[k],
 !.queryCut=s.cut, !.queryCompaction=s.compactedAt, !.published=s.clock-1, !.operation=op]
Finish == /\ s.phase="query"
 /\ s'=[s EXCEPT !.phase="done", !.allowed=s.queryBound>0 /\
 (s.operation="edit" \/ ~(s.queryCompaction>s.queryBound /\ s.queryCut>s.queryBound))]
Reload == /\ s.phase="idle" /\ s.generation=0
 /\ s'=[s EXCEPT !.bindings=[k\in Keys|->0], !.generation=1]
Next == (\E k\in Keys:Bind(k))
 \/(\E c\in 0..4,ok\in BOOLEAN:Compact(c,ok))\/(\E k\in Keys,op\in{"write","edit"}:Begin(k,op))\/Finish\/Reload
Spec == Init /\ [][Next]_s
NoCrossBranchOrFileBinding == s.phase="query"=>s.queryBound=s.bindings[s.queryKey]
BothQueriesUseCut == s.phase#"idle"=>s.queryCompaction<=s.published
WriteNeedsContext == s.phase="done" /\ s.allowed /\ s.operation="write"=>~(s.queryCompaction>s.queryBound /\ s.queryCut>s.queryBound)
EditNeedsBindingOnly == s.phase="done" /\ s.operation="edit"=>s.allowed=(s.queryBound>0)
ReloadClearsBindings == [][Reload=>\A k\in Keys:s.bindings'[k]=0]_s
UnrelatedBindingsUnchanged == [][\A k\in Keys:Bind(k)=>\A other\in Keys\{k}:s.bindings'[other]=s.bindings[other]]_s
=============================================================================
