----------------------- MODULE ApprovalOrdering -----------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
CONSTANT MaxEvents
VARIABLES history, published, consumed, policy, pending, responders,
          accepted, phase, cancelled, closed, captured, autoAt, advisorAsk,
          admissionPolicy, admissionPublished, admissionCancelled
vars == <<history, published, consumed, policy, pending, responders,
          accepted, phase, cancelled, closed, captured, autoAt, advisorAsk,
          admissionPolicy, admissionPublished, admissionCancelled>>
Policies == {"ask", "allow", "deny"}
Event(k, v) == [kind |-> k, value |-> v]
Room == Len(history) < MaxEvents
Init == /\ history = <<Event("request", "ask")>>
        /\ published = 0 /\ consumed = 0 /\ policy = "ask"
        /\ pending = TRUE /\ responders = [i \in 1..2 |-> "new"]
        /\ accepted = 0 /\ phase = "waiting"
        /\ cancelled = FALSE /\ closed = FALSE /\ captured = 0
        /\ autoAt = 0 /\ advisorAsk \in BOOLEAN
        /\ admissionPolicy = "ask" /\ admissionPublished = 0
        /\ admissionCancelled = FALSE
PolicyWrite(v) ==
    /\ ~closed /\ Room
    /\ history' = Append(history, Event("policy", v))
    /\ UNCHANGED <<published, consumed, policy, pending, responders, accepted,
                   phase, cancelled, closed, captured, autoAt, advisorAsk,
                   admissionPolicy, admissionPublished, admissionCancelled>>
RespondBegin(i) ==
    /\ ~closed /\ pending /\ responders[i] = "new"
    /\ responders' = [responders EXCEPT ![i] = "checked"]
    /\ UNCHANGED <<history, published, consumed, policy, pending, accepted,
                   phase, cancelled, closed, captured, autoAt, advisorAsk,
                   admissionPolicy, admissionPublished, admissionCancelled>>
RespondCommit(i, v) ==
    /\ ~closed /\ Room /\ responders[i] = "checked"
    /\ history' = Append(history, Event("response", v))
    /\ responders' = [responders EXCEPT ![i] = "committed"]
    /\ UNCHANGED <<published, consumed, policy, pending, accepted, phase,
                   cancelled, closed, captured, autoAt, advisorAsk,
                   admissionPolicy, admissionPublished, admissionCancelled>>
Publish ==
    /\ published < Len(history) /\ published' = published + 1
    /\ UNCHANGED <<history, consumed, policy, pending, responders, accepted,
                   phase, cancelled, closed, captured, autoAt, advisorAsk,
                   admissionPolicy, admissionPublished, admissionCancelled>>
Dispatch ==
    /\ ~closed /\ consumed < published
    /\ LET e == history[consumed + 1]
           wins == e.kind = "response" /\ pending
       IN /\ consumed' = consumed + 1
          /\ policy' = IF e.kind = "policy" THEN e.value ELSE policy
          /\ pending' = IF wins THEN FALSE ELSE pending
          /\ accepted' = IF wins /\ ~cancelled THEN accepted + 1 ELSE accepted
          /\ phase' = IF wins THEN
                         IF cancelled THEN "cancelled"
                         ELSE IF e.value = "deny" THEN "denied" ELSE "manual"
                      ELSE phase
    /\ UNCHANGED <<history, published, responders, cancelled, closed, captured,
                   autoAt, advisorAsk, admissionPolicy, admissionPublished,
                   admissionCancelled>>
CaptureAuto ==
    /\ ~closed /\ pending /\ consumed = Len(history)
    /\ policy = "deny" \/ (policy = "allow" /\ ~advisorAsk)
    /\ captured' = consumed
    /\ UNCHANGED <<history, published, consumed, policy, pending, responders,
                   accepted, phase, cancelled, closed, autoAt, advisorAsk,
                   admissionPolicy, admissionPublished, admissionCancelled>>
CommitAuto ==
    /\ ~closed /\ ~cancelled /\ pending /\ Room
    /\ captured = Len(history) /\ captured = consumed
    /\ history' = Append(history, Event("terminal", policy))
    /\ autoAt' = Len(history') /\ pending' = FALSE
    /\ phase' = IF policy = "allow" THEN "auto" ELSE "denied"
    /\ UNCHANGED <<published, consumed, policy, responders, accepted, cancelled,
                   closed, captured, advisorAsk, admissionPolicy,
                   admissionPublished, admissionCancelled>>
FinishAdmission ==
    /\ ~cancelled
    /\ phase = "manual" \/ (phase = "auto" /\ published >= autoAt)
    /\ phase' = IF policy = "deny" \/ (phase = "auto" /\ policy # "allow")
                 THEN "denied" ELSE "allowed"
    /\ admissionPolicy' = policy /\ admissionPublished' = published
    /\ admissionCancelled' = cancelled
    /\ UNCHANGED <<history, published, consumed, policy, pending, responders,
                   accepted, cancelled, closed, captured, autoAt, advisorAsk>>
Cancel ==
    /\ ~cancelled /\ cancelled' = TRUE
    /\ pending' = FALSE
    /\ phase' = IF phase = "allowed" THEN phase ELSE "cancelled"
    /\ UNCHANGED <<history, published, consumed, policy, responders, accepted,
                   closed, captured, autoAt, advisorAsk, admissionPolicy,
                   admissionPublished, admissionCancelled>>
Close ==
    /\ ~closed /\ closed' = TRUE /\ pending' = FALSE
    /\ phase' = IF phase = "waiting" THEN "closed" ELSE phase
    /\ UNCHANGED <<history, published, consumed, policy, responders, accepted,
                   cancelled, captured, autoAt, advisorAsk, admissionPolicy,
                   admissionPublished, admissionCancelled>>
Next == (\E v \in Policies : PolicyWrite(v))
     \/ (\E i \in 1..2 : RespondBegin(i))
     \/ (\E i \in 1..2, v \in {"allow", "deny"} : RespondCommit(i, v))
     \/ Publish \/ Dispatch \/ CaptureAuto \/ CommitAuto
     \/ FinishAdmission \/ Cancel \/ Close
Spec == Init /\ [][Next]_vars
TypeOK == /\ history \in Seq([kind : {"request", "policy", "response", "terminal"}, value : Policies])
          /\ consumed \in 0..published /\ published \in 0..Len(history)
          /\ policy \in Policies /\ pending \in BOOLEAN
          /\ responders \in [1..2 -> {"new", "checked", "committed"}]
          /\ accepted \in Nat
          /\ phase \in {"waiting", "manual", "auto", "allowed", "denied", "cancelled", "closed"}
          /\ cancelled \in BOOLEAN /\ closed \in BOOLEAN
          /\ captured \in 0..MaxEvents
          /\ autoAt \in 0..Len(history) /\ advisorAsk \in BOOLEAN
          /\ admissionPolicy \in Policies /\ admissionPublished \in 0..Len(history)
          /\ admissionCancelled \in BOOLEAN
SingleWinner == accepted <= 1
NoAutoOvertake == autoAt > 0 => \A i \in 1..(autoAt - 1) : history[i].kind # "response"
PublishedAdmission == phase = "allowed" /\ autoAt > 0 => admissionPublished >= autoAt
PolicyAdmission == phase = "allowed" =>
    /\ admissionPolicy # "deny"
    /\ (autoAt > 0 => admissionPolicy = "allow" /\ ~advisorAsk)
    /\ ~admissionCancelled
NoPendingAfterSettlement == phase # "waiting" => ~pending
=============================================================================
