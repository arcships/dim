-------------------- MODULE TimelinePublication --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
CONSTANTS MaxBatches, MaxBatchSize
VARIABLE s
Modes == {"unchanged", "transformed", "failed"}
End(b) == IF b = 0 THEN 0 ELSE s.ends[b]
Allocated == Len(s.events)
Boundaries == {0} \cup {s.ends[b] : b \in 1..Len(s.ends)}
Init == s = [events |-> <<>>, ends |-> <<>>, accepted |-> 0,
             resolved |-> 0, callbacks |-> 0, durable |-> 0,
             published |-> 0, consumed |-> 0, inflight |-> 0,
             storeFailed |-> FALSE, failed |-> FALSE, closing |-> FALSE,
             closed |-> FALSE,
             readCut |-> 0, readRows |-> <<>>]
CommitBatch(w, n, modes, accept) ==
    /\ ~s.closing /\ ~s.failed /\ Len(s.ends) < MaxBatches
    /\ LET batch == Len(s.ends) + 1
           rows == [i \in 1..n |-> [sequence |-> Allocated + i,
                     batch |-> batch, writer |-> w, mode |-> modes[i],
                     value |-> IF modes[i] = "transformed" THEN 1 ELSE 0]]
       IN s' = [s EXCEPT !.events = @ \o rows,
                        !.ends = Append(@, Allocated + n),
                        !.accepted = IF accept THEN batch ELSE @,
                        !.failed = ~accept, !.closed = ~accept]
Resolve(last, ok) ==
    /\ s.resolved = s.callbacks
    /\ last \in (s.resolved + 1)..s.accepted
    /\ (~s.storeFailed \/ ~ok)
    /\ s' = [s EXCEPT !.resolved = last,
                     !.durable = IF ok THEN End(last) ELSE @,
                     !.storeFailed = @ \/ ~ok]
Store == \E last \in 1..MaxBatches, ok \in BOOLEAN: Resolve(last, ok)
Callback ==
    /\ s.callbacks < s.resolved
    /\ LET b == s.callbacks + 1
           ok == End(b) <= s.durable
       IN s' = [s EXCEPT !.callbacks = b,
                        !.published = IF ~s.failed /\ ok THEN End(b) ELSE @,
                        !.failed = @ \/ ~ok,
                        !.closed = IF s.failed \/ ~ok THEN TRUE
                                   ELSE s.closing /\ End(b) = Allocated]
Fetch ==
    /\ ~s.failed /\ s.inflight = 0 /\ s.consumed < s.published
    /\ s' = [s EXCEPT !.inflight = s.consumed + 1]
Deliver ==
    /\ s.inflight # 0
    /\ s' = [s EXCEPT !.consumed = s.inflight, !.inflight = 0]
ReadExternal ==
    s' = [s EXCEPT !.readCut = s.published,
                     !.readRows = SubSeq(s.events, 1, s.published)]
Close ==
    /\ ~s.closing
    /\ s' = [s EXCEPT !.closing = TRUE,
                     !.closed = s.failed \/ s.published = Allocated]
Next == (\E w \in {1, 2}, n \in 1..MaxBatchSize:
          \E modes \in [1..n -> Modes], accept \in BOOLEAN:
             CommitBatch(w, n, modes, accept))
        \/ Store \/ Callback \/ Fetch \/ Deliver
        \/ ReadExternal \/ Close
Spec == Init /\ [][Next]_s /\ WF_s(Store) /\ WF_s(Callback)
TypeOK ==
    /\ s.accepted \in 0..Len(s.ends)
    /\ s.callbacks \in 0..s.resolved /\ s.resolved \in 0..s.accepted
    /\ s.consumed \in 0..s.published /\ s.published \in 0..s.durable
    /\ s.durable \in 0..Allocated
    /\ s.inflight \in {0, s.consumed + 1}
    /\ s.inflight <= s.published
SequenceAndBatch ==
    /\ \A i \in 1..Allocated: s.events[i].sequence = i
    /\ \A b \in 1..Len(s.ends):
        /\ End(b) > End(b - 1)
        /\ \A i \in (End(b - 1) + 1)..End(b):
            /\ s.events[i].batch = b
            /\ s.events[i].writer = s.events[End(b)].writer
AtomicVisibility == s.published \in Boundaries /\ s.durable \in Boundaries
TransformOutcome == \A i \in 1..Allocated:
    s.events[i].value = IF s.events[i].mode = "transformed" THEN 1 ELSE 0
ExternalPrefix ==
    /\ s.readCut <= s.published
    /\ s.readRows = SubSeq(s.events, 1, s.readCut)
ClosedMeaning == s.closed => s.failed \/ (s.closing /\ s.published = Allocated)
FailureFreezesPublication == [][s.failed => s.published' = s.published]_s
NoAdmissionAfterClose == [][s.closing \/ s.failed => s.events' = s.events]_s
DeliveryIsOrdered == [][Deliver => s.consumed' = s.consumed + 1]_s
ClosingSettles == s.closing ~> s.closed
=============================================================================
