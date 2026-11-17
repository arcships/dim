-------------------- MODULE PromptProjection --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Keys == {<<1,1>>,<<1,2>>,<<2,1>>}
Init == \E legacy\in BOOLEAN:s=[entries|->[k\in Keys|->0],active|->{1,2},history|-><<>>, source|->1,loaded|->1,renderedSource|->1,guide|->0,renderedGuide|->0,legacy|->legacy]
Entry(k,value) == /\ Len(s.history)<3 /\ k[2]\in s.active /\ s'=[s EXCEPT !.entries[k]=value, !.history=Append(@,[key|->k,value|->value])]
Retire(scope) == /\ scope\in s.active /\ Len(s.history)<3 /\ s'=[s EXCEPT !.active=@\{scope}, !.entries=[k\in Keys|->IF k[2]=scope THEN 0 ELSE s.entries[k]]]
At(k) == LET positions=={i\in 1..Len(s.history):s.history[i].key=k} IN IF k[2]\notin s.active \/ positions={} THEN 0 ELSE s.history[CHOOSE i\in positions:\A j\in positions:i>=j].value
Restore == s'=[s EXCEPT !.entries=[k\in Keys|->At(k)]]
Load == s'=[s EXCEPT !.loaded=2]
Adopt == /\ ~s.legacy /\ s'=[s EXCEPT !.source=s.loaded, !.renderedSource=s.loaded, !.renderedGuide=s.guide]
Guide == s'=[s EXCEPT !.guide=1]
Rerender == s'=[s EXCEPT !.renderedSource=IF s.legacy THEN @ ELSE s.source, !.renderedGuide=IF s.legacy THEN @ ELSE s.guide]
Next == (\E k\in Keys,v\in 0..2:Entry(k,v))\/(\E scope\in 1..2:Retire(scope))\/Restore\/Load\/Adopt\/Guide\/Rerender
Spec == Init /\ [][Next]_s
InactiveScopeAbsent == \A k\in Keys:k[2]\notin s.active=>s.entries[k]=0
FullIdentityLatest == \A k\in Keys:s.entries[k]=At(k)
GuideUsesAdoptedSource == [][Rerender /\ ~s.legacy=>s.renderedSource'=s.source]_s
LegacyKeepsAdoptedSystem == [][Rerender /\ s.legacy=>s.renderedSource'=s.renderedSource /\ s.renderedGuide'=s.renderedGuide]_s
LoadDoesNotAdopt == [][Load=>s.source'=s.source /\ s.renderedSource'=s.renderedSource]_s
=============================================================================
