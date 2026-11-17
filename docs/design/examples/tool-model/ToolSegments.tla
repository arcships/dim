-------------------------- MODULE ToolSegments --------------------------
EXTENDS Naturals, FiniteSets, TLC
CONSTANT Calls
VARIABLES concurrent, segment, phase, remaining, head, cancelled
vars == <<concurrent, segment, phase, remaining, head, cancelled>>
RECURSIVE Segment(_)
Segment(i) == IF i = 1 THEN 1
              ELSE IF concurrent[i-1] /\ concurrent[i]
                   THEN Segment(i-1) ELSE Segment(i-1) + 1
Init == /\ concurrent \in [1..Calls -> BOOLEAN]
        /\ segment = [i \in 1..Calls |-> Segment(i)]
        /\ phase = [i \in 1..Calls |-> "new"]
        /\ remaining = [s \in 1..Calls |-> Cardinality({i \in 1..Calls : segment[i] = s})]
        /\ head = 1 /\ cancelled = FALSE
Admit(i) ==
    /\ ~cancelled /\ phase[i] = "new"
    /\ phase' = [phase EXCEPT ![i] = "admitted"]
    /\ UNCHANGED <<concurrent, segment, remaining, head, cancelled>>
Run(i) ==
    /\ ~cancelled /\ phase[i] = "admitted" /\ segment[i] <= head
    /\ phase' = [phase EXCEPT ![i] = "running"]
    /\ UNCHANGED <<concurrent, segment, remaining, head, cancelled>>
Settle(i) ==
    /\ phase[i] # "settled"
    /\ phase' = [phase EXCEPT ![i] = "settled"]
    /\ remaining' = [remaining EXCEPT ![segment[i]] = @ - 1]
    /\ UNCHANGED <<concurrent, segment, head, cancelled>>
Advance ==
    /\ head <= Calls /\ remaining[head] = 0
    /\ head' = head + 1
    /\ UNCHANGED <<concurrent, segment, phase, remaining, cancelled>>
Cancel == /\ ~cancelled /\ cancelled' = TRUE
          /\ UNCHANGED <<concurrent, segment, phase, remaining, head>>
Next == (\E i \in 1..Calls : Admit(i) \/ Run(i) \/ Settle(i)) \/ Advance \/ Cancel
Spec == Init /\ [][Next]_vars
TypeOK == /\ concurrent \in [1..Calls -> BOOLEAN]
          /\ segment \in [1..Calls -> 1..Calls]
          /\ phase \in [1..Calls -> {"new", "admitted", "running", "settled"}]
          /\ remaining \in [1..Calls -> 0..Calls]
          /\ head \in 1..(Calls+1) /\ cancelled \in BOOLEAN
CountsMatch == \A s \in 1..Calls :
    remaining[s] = Cardinality({i \in 1..Calls : segment[i] = s /\ phase[i] # "settled"})
PrefixOnly == \A i \in 1..Calls : segment[i] < head => phase[i] = "settled"
OrderedExecution == \A i,j \in 1..Calls :
    phase[i] = "running" /\ segment[j] < segment[i] => phase[j] = "settled"
ExclusiveRunsAlone == \A i,j \in 1..Calls :
    i # j /\ phase[i] = "running" /\ phase[j] = "running" => concurrent[i] /\ concurrent[j]
NoStartAfterCancel == [][cancelled => \A i \in 1..Calls :
    phase[i] # "running" => phase'[i] # "running"]_vars
=============================================================================
