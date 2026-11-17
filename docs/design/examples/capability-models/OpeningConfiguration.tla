-------------------- MODULE OpeningConfiguration --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[snapshot|->1,openingSnapshot|->0,queue|-><<>>,adopted|->0,receipt|->0,published|->{},failed|->FALSE,requests|-><<>>]
Opening == /\ s.openingSnapshot=0 /\ s'=[s EXCEPT !.openingSnapshot=s.snapshot, !.queue=Append(@,[opening|->TRUE,source|->s.snapshot])]
Change == /\ s.openingSnapshot#0 /\ s.snapshot=1 /\ s'=[s EXCEPT !.snapshot=2, !.queue=Append(@,[opening|->FALSE,source|->2])]
Adopt(ok) == /\ s.queue# <<>> /\ ~s.failed /\ (s.adopted=0 \/ s.adopted\in s.published)
 /\ LET item==Head(s.queue) IN s'=[s EXCEPT !.queue=Tail(@), !.adopted=IF ok THEN item.source ELSE @,
 !.receipt=IF item.opening /\ ok THEN item.source ELSE @, !.failed=item.opening /\ ~ok]
Publish(source) == /\ source=s.adopted /\ source#0 /\ s'=[s EXCEPT !.published=@\cup{source}]
Request == /\ s.receipt\in s.published /\ s.adopted\in s.published /\ ~s.failed /\ Len(s.requests)<2
 /\ s'=[s EXCEPT !.requests=Append(@,s.adopted)]
Next == Opening\/Change\/(\E b\in BOOLEAN:Adopt(b))\/(\E source\in 1..2:Publish(source))\/Request
Spec == Init /\ [][Next]_s
ReceiptKeepsOpeningSnapshot == s.receipt#0=>s.receipt=s.openingSnapshot
FailedOpeningCannotRequest == s.failed=>s.requests= <<>>
FirstRequestWaitsForReceipt == s.requests# <<>>=>s.receipt\in s.published
DynamicChangeDoesNotRewriteReceipt == [][Change=>s.receipt'=s.receipt /\ s.openingSnapshot'=s.openingSnapshot]_s
=============================================================================
