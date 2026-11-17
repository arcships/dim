---------------------------- MODULE WaitRecovery ----------------------------
EXTENDS Naturals, Sequences, FiniteSets

CONSTANT MaxEvents
Kinds == {"start", "end", "arrival", "other"}
VARIABLES history, position, open, phase, cutoff, base, delivered
vars == <<history, position, open, phase, cutoff, base, delivered>>

Boundaries(h, c) == {i \in 1..c : h[i] \in {"start", "end"}}
OpenAt(h, c) ==
    IF Boundaries(h, c) = {} THEN FALSE
    ELSE h[CHOOSE i \in Boundaries(h, c) :
               \A j \in Boundaries(h, c) : j <= i] = "start"

Init ==
    /\ history = <<>>
    /\ position = 0
    /\ open = FALSE
    /\ phase = "live"
    /\ cutoff = 0
    /\ base = 0
    /\ delivered = <<>>

AppendFact ==
    /\ Len(history) < MaxEvents
    /\ \E e \in Kinds : history' = Append(history, e)
    /\ UNCHANGED <<position, open, phase, cutoff, base, delivered>>

Deliver ==
    /\ phase = "live"
    /\ position < Len(history)
    /\ LET e == history[position + 1] IN
           open' = IF e = "start" THEN TRUE
                   ELSE IF e = "end" THEN FALSE ELSE open
    /\ position' = position + 1
    /\ delivered' = Append(delivered, position + 1)
    /\ UNCHANGED <<history, phase, cutoff, base>>

BeginRecovery ==
    /\ phase = "live"
    /\ phase' = "holding"
    /\ UNCHANGED <<history, position, open, cutoff, base, delivered>>

TakeCut ==
    /\ phase = "holding"
    /\ cutoff' = Len(history)
    /\ phase' = "querying"
    /\ UNCHANGED <<history, position, open, base, delivered>>

Recover ==
    /\ phase = "querying"
    /\ open' = OpenAt(history, cutoff)
    /\ position' = cutoff
    /\ base' = cutoff
    /\ delivered' = <<>>
    /\ phase' = "live"
    /\ UNCHANGED <<history, cutoff>>

Next == AppendFact \/ Deliver \/ BeginRecovery \/ TakeCut \/ Recover
Spec == Init /\ [][Next]_vars /\ WF_vars(TakeCut) /\ WF_vars(Recover)

TypeOK ==
    /\ history \in Seq(Kinds)
    /\ Len(history) <= MaxEvents
    /\ position \in 0..Len(history)
    /\ cutoff \in 0..Len(history)
    /\ base \in 0..position
    /\ open \in BOOLEAN
    /\ phase \in {"live", "holding", "querying"}
    /\ delivered \in Seq(1..MaxEvents)

Projection == phase = "live" => open = OpenAt(history, position)
ExactSuffix == phase = "live" =>
    delivered = [i \in 1..(position - base) |-> base + i]
RecoveryCompletes == (phase # "live") ~> (phase = "live")
=============================================================================
