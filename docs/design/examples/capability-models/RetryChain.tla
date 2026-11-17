-------------------- MODULE RetryChain --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
RECURSIVE Sum(_)
Sum(q) == IF Len(q)=0 THEN 0 ELSE Head(q)+Sum(Tail(q))
Init == s=[request|->1, phase|->"ready", fresh|->TRUE, attempts|->0, waited|->0, malformed|->0,
 backoff|->0, irreversible|->FALSE, announced|->FALSE, continued|->FALSE,
 allowed|->FALSE, calls|-><<>>, sleeps|-><<>>, planned|-><<>>, chain|->[from|->0,attempts|->0,waited|->0],
 cancelled|->FALSE, localRetries|-><<>>, exhausted|->FALSE]
Invoke == /\ s.phase="ready" /\ ~s.cancelled
 /\ LET spent==s.attempts>=8 \/ (s.fresh /\ s.waited>=300) IN
 s'=[s EXCEPT !.phase=IF spent THEN "ended" ELSE "running", !.exhausted=spent,
 !.attempts=IF spent THEN @ ELSE @+1, !.calls=IF spent THEN @ ELSE Append(@,s.request)]
Announce == /\ s.phase="running" /\ s'=[s EXCEPT !.irreversible=TRUE, !.announced=TRUE]
Failure(kind,delay,elapsed,forbidden) == /\ s.phase="running"
 /\ (IF delay=0 THEN elapsed=0 ELSE elapsed>=delay)
 /\ LET retry==~forbidden /\ ~s.irreversible /\ s.attempts<8 /\ delay<=(IF s.waited>=300 THEN 0 ELSE 300-s.waited)
             /\ kind\in{"transient","malformed"} /\ (kind#"malformed" \/ s.malformed=0)
 IN s'=[s EXCEPT !.phase=IF retry THEN "ready" ELSE "ended",
 !.waited=IF retry THEN @+elapsed ELSE @, !.sleeps=IF retry THEN Append(@,elapsed) ELSE @,
 !.planned=IF retry THEN Append(@,[delay|->delay,before|->s.waited]) ELSE @,
 !.fresh=FALSE, !.backoff=IF retry THEN @+1 ELSE @,
 !.malformed=IF retry /\ kind="malformed" THEN @+1 ELSE @,
 !.localRetries=IF retry /\ kind="malformed" THEN Append(@,s.request) ELSE @,
 !.allowed=~forbidden /\ kind#"permanent"]
Handoff == /\ s.phase="ended" /\ s.announced /\ s.allowed /\ ~s.continued /\ ~s.cancelled
 /\ s'=[s EXCEPT !.phase="preparing", !.continued=TRUE,
 !.chain=[from|->s.request,attempts|->s.attempts,waited|->s.waited]]
Compact == /\ s.phase="preparing" /\ s'=[s EXCEPT !.phase="compacting"]
Compacted == /\ s.phase="compacting" /\ s'=[s EXCEPT !.phase="prepared"]
Prepared == /\ s.phase\in{"preparing","prepared"} /\ ~s.cancelled
 /\ s'=[s EXCEPT !.request=2, !.phase="ready", !.fresh=TRUE, !.attempts=s.chain.attempts, !.waited=s.chain.waited,
 !.malformed=0, !.backoff=0, !.irreversible=FALSE, !.announced=FALSE, !.allowed=FALSE]
PreparationFailed == /\ s.phase\in{"preparing","compacting","prepared"}
 /\ s'=[s EXCEPT !.phase="failed"]
Cancel == s'=[s EXCEPT !.cancelled=TRUE, !.phase="cancelled"]
Next == Invoke\/Announce\/(\E k\in{"transient","malformed","output_limit","permanent"},d\in{0,150,300,301},elapsed\in{0,150,300,301},f\in BOOLEAN:Failure(k,d,elapsed,f))
 \/Handoff\/Compact\/Compacted\/Prepared\/PreparationFailed\/Cancel
Spec == Init /\ [][Next]_s
ChainAttemptBudget == Len(s.calls)<=8 /\ s.attempts=Len(s.calls)
ChainWaitBudget == s.waited=Sum(s.sleeps) /\ \A i\in 1..Len(s.planned):s.planned[i].delay<=(IF s.planned[i].before>=300 THEN 0 ELSE 300-s.planned[i].before)
RequestLocalMalformed == \A r\in 1..2:Cardinality({i\in 1..Len(s.localRetries):s.localRetries[i]=r})<=1
HandoffKeepsTotals == s.phase\in{"preparing","compacting","prepared"}=>s.chain.attempts=s.attempts /\ s.chain.waited=s.waited
NoCallOnSpentChain == [][s.attempts>=8 \/ (s.fresh /\ s.waited>=300)=>s.calls'=s.calls]_s
LocalResetOnly == [][Prepared=>s.malformed'=0 /\ s.backoff'=0 /\ s.attempts'=s.attempts /\ s.waited'=s.waited]_s
NoCallAfterCancel == [][s.cancelled=>s.calls'=s.calls]_s
=============================================================================
