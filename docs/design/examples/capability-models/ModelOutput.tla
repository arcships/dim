-------------------- MODULE ModelOutput --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[phase |-> "idle", started |-> FALSE, attempt |-> 0, irreversible |-> FALSE,
 cancelled |-> FALSE, released |-> TRUE, outputs |-> <<>>, terminals |-> 0,
 pendingTools |-> {}, toolErrors |-> {}, outcome |-> "none"]
Start == /\ ~s.started /\ s'=[s EXCEPT !.started=TRUE, !.phase="running", !.attempt=1, !.released=FALSE]
Output(kind) == /\ s.phase="running" /\ ~s.cancelled /\ Len(s.outputs)<2
 /\ s'=[s EXCEPT !.outputs=Append(@,[kind|->kind,attempt|->s.attempt]), !.irreversible=TRUE,
 !.pendingTools=IF kind="tool" THEN @\cup{Len(s.outputs)+1} ELSE @]
Retry == /\ s.phase="running" /\ ~s.cancelled /\ ~s.irreversible /\ s.attempt<8
 /\ s'=[s EXCEPT !.attempt=@+1]
Fail(reason) == /\ s.phase="running" /\ s'=[s EXCEPT !.phase="settling", !.outcome=reason]
Cancel == /\ s.phase="running" /\ s'=[s EXCEPT !.cancelled=TRUE, !.phase="settling", !.outcome="cancelled"]
Release == /\ s.phase="settling" /\ s'=[s EXCEPT !.released=TRUE]
Terminal == /\ s.phase="settling" /\ s.released
 /\ s'=[s EXCEPT !.phase="ended", !.terminals=@+1, !.toolErrors=IF s.outcome="success" THEN {} ELSE s.pendingTools]
Next == Start\/(\E k\in{"text","thinking","tool","content","usage"}:Output(k))\/Retry
 \/(\E r\in{"error","output_limit","success"}:Fail(r))\/Cancel\/Release\/Terminal
Spec == Init /\ [][Next]_s
OneTerminal == s.terminals<=1
ReleaseBeforeEnd == s.terminals>0 => s.released
NoReplayOutput == \A i\in 1..Len(s.outputs):s.outputs[i].attempt=s.attempt
ToolsSettled == s.phase="ended" /\ s.outcome#"success" => s.toolErrors=s.pendingTools
OutputLimitStops == s.outcome="output_limit" => s.phase\in{"settling","ended"}
BoundedRetry == s.attempt<=8
=============================================================================
