-------------------- MODULE LoopProtocol --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[turn |-> 0, next |-> 1, phase |-> "idle", driver |-> FALSE,
 request |-> 0, requests |-> 0, stopped |-> FALSE, failed |-> FALSE, ended |-> {}, starts |-> <<>>,
 invocations |-> <<>>, toolCalls |-> {}, reloaded |-> FALSE]
Input(mode) == s'=[s EXCEPT !.driver=@\/(mode#"append")]
Start == /\ s.turn=0 /\ s.phase="idle" /\ s.driver /\ s.next<=2 /\ s.requests<4
 /\ s'=[s EXCEPT !.turn=s.next, !.next=@+1, !.phase="preparing", !.request=s.requests+1, !.requests=@+1,
 !.driver=FALSE, !.stopped=FALSE, !.failed=FALSE, !.reloaded=FALSE,
 !.starts=Append(@,s.next)]
Prepared(r) == /\ r=s.request /\ s.phase="preparing" /\ ~s.stopped
 /\ s'=[s EXCEPT !.phase="model", !.invocations=Append(@,r)]
Output(r,tools) == /\ r=s.request /\ s.phase="model"
 /\ s'=[s EXCEPT !.phase=IF tools /\ ~s.stopped THEN "tools" ELSE "ending",
 !.toolCalls=IF tools /\ ~s.stopped THEN @\cup{r} ELSE @]
ToolResult(r,reload) == /\ r=s.request /\ s.phase="tools"
 /\ s'=[s EXCEPT !.phase=IF ~reload /\ ~s.stopped /\ s.requests<4 THEN "preparing" ELSE "ending",
 !.request=IF ~reload /\ ~s.stopped /\ s.requests<4 THEN s.requests+1 ELSE @,
 !.requests=IF ~reload /\ ~s.stopped /\ s.requests<4 THEN @+1 ELSE @, !.failed=@\/reload, !.reloaded=reload]
Interrupt == /\ s.turn#0 /\ s'=[s EXCEPT !.stopped=TRUE, !.phase="ending"]
Reload == /\ s.turn#0 /\ s'=[s EXCEPT !.stopped=TRUE, !.phase="ending", !.failed=TRUE, !.reloaded=TRUE]
End == /\ s.phase="ending" /\ s.turn#0
 /\ s'=[s EXCEPT !.ended=@\cup{s.turn}, !.turn=0, !.phase="idle", !.request=0]
Compact == /\ s.turn=0 /\ s.phase="idle" /\ s'=[s EXCEPT !.phase="compacting"]
Compacted == /\ s.phase="compacting" /\ s'=[s EXCEPT !.phase="idle"]
Next == (\E m\in{"queue","steer","append"}:Input(m)) \/ Start
 \/ (\E r\in 1..4:Prepared(r)) \/ (\E r\in 1..4,b\in BOOLEAN:Output(r,b)\/ToolResult(r,b))
 \/ Interrupt \/ Reload \/ End \/ Compact \/ Compacted
Spec == Init /\ [][Next]_s
OneOpenTurn == Cardinality({s.starts[i]:i\in 1..Len(s.starts)}\s.ended)<=1
NoRestart == Len(s.starts)=Cardinality({s.starts[i]:i\in 1..Len(s.starts)})
NoRepeatModel == Len(s.invocations)=Cardinality({s.invocations[i]:i\in 1..Len(s.invocations)})
CompactionExcludesTurn == s.phase="compacting" => s.turn=0
ReloadFailsTurn == s.reloaded => s.failed
StartedBeforeTools == s.toolCalls \subseteq {s.invocations[i]:i\in 1..Len(s.invocations)}
=============================================================================
