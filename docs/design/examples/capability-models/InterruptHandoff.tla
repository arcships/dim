-------------------- MODULE InterruptHandoff --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
CONSTANT Turns
VARIABLE s
Actors == {"loop", "tool"}
Event(k,t) == [kind |-> k, turn |-> t]
Init == s = [log |-> <<Event("start",1)>>, published |-> 0, cursor |-> 0,
 todo |-> {}, seen |-> [a \in Actors |-> 0], turn |-> 1,
 model |-> {}, interrupts |-> {}, requested |-> {}, ended |-> {},
 producerStopped |-> FALSE, consumerStopped |-> FALSE,
 work |-> "none", current |-> 0, cancelled |-> FALSE,
 starts |-> {}, results |-> {}, suppressed |-> {}, late |-> {}]
Publish == /\ s.published < Len(s.log)
 /\ s' = [s EXCEPT !.published = @ + 1]
Begin == /\ s.todo = {} /\ s.cursor < s.published
 /\ s' = [s EXCEPT !.todo = Actors]
Model == /\ s.turn \notin s.model /\ s.seen["loop"] > 0
 /\ \E i \in 1..s.seen["loop"]: s.log[i] = Event("start",s.turn)
 /\ s' = [s EXCEPT !.log = Append(@,Event("model",s.turn)), !.model = @ \cup {s.turn}]
Interrupt == /\ s.turn \notin s.interrupts /\ s.turn \notin s.ended
 /\ s' = [s EXCEPT !.log = Append(@,Event("interrupt",s.turn)), !.interrupts = @ \cup {s.turn}]
Deliver(a) == /\ a \in s.todo
 /\ LET e == s.log[s.cursor+1]
        request == a="loop" /\ e.kind="model" /\ ~s.producerStopped
        end == a="loop" /\ (e.kind="result" \/ (e.kind="model" /\ s.producerStopped))
        reject == a="tool" /\ e.kind="request" /\ s.consumerStopped
 IN s' = [s EXCEPT
 !.seen[a] = s.cursor+1, !.todo = @ \ {a},
 !.cursor = IF s.todo={a} THEN @+1 ELSE @,
 !.producerStopped = IF a="loop" THEN CASE e.kind="interrupt" -> TRUE
                    [] e.kind \in {"start","end"} -> FALSE [] OTHER -> @ ELSE @,
 !.consumerStopped = IF a="tool" THEN CASE e.kind="interrupt" -> TRUE
                    [] e.kind \in {"start","end"} -> FALSE [] OTHER -> @ ELSE @,
 !.cancelled = IF a="tool" /\ e.kind="interrupt" /\ s.work="pending" THEN TRUE
              ELSE IF a="tool" /\ e.kind="request" THEN s.consumerStopped ELSE @,
 !.work = IF a="tool" /\ e.kind="request" THEN IF reject THEN "none" ELSE "pending" ELSE @,
 !.current = IF a="tool" /\ e.kind="request" THEN e.turn ELSE @,
 !.requested = IF request THEN @ \cup {e.turn} ELSE @,
 !.late = IF request /\ e.turn \in s.interrupts THEN @ \cup {e.turn} ELSE @,
 !.ended = IF end THEN @ \cup {e.turn} ELSE @,
 !.suppressed = IF reject THEN @ \cup {e.turn} ELSE @,
 !.results = IF reject THEN @ \cup {e.turn} ELSE @,
 !.log = IF request THEN Append(@,Event("request",e.turn))
         ELSE IF end THEN Append(@,Event("end",e.turn))
         ELSE IF reject THEN Append(@,Event("result",e.turn)) ELSE @]
Invoke == /\ s.work="pending" /\ ~s.cancelled /\ s.current \notin s.starts
 /\ s' = [s EXCEPT !.starts = @ \cup {s.current}]
Finish == /\ s.work="pending" /\ (s.current \in s.starts \/ s.cancelled)
 /\ s' = [s EXCEPT !.work="none", !.results=@ \cup {s.current},
 !.log=Append(@,Event("result",s.current))]
NextTurn == /\ s.turn < Turns /\ s.todo={} /\ s.cursor>0
 /\ s.log[s.cursor]=Event("end",s.turn)
 /\ s'=[s EXCEPT !.turn=@+1, !.log=Append(@,Event("start",s.turn+1))]
Next == Publish \/ Begin \/ Model \/ Interrupt \/ (\E a \in Actors: Deliver(a)) \/ Invoke \/ Finish \/ NextTurn
Spec == Init /\ [][Next]_s
LateRequestsNeverInvoke == s.late \intersect s.starts = {}
SuppressedRequestsSettle == s.suppressed \subseteq s.results
TurnEndWaitsForResult == s.ended \intersect s.requested \subseteq s.results
ConsumedPrefix == /\ \A a \in Actors: s.cursor <= s.seen[a] /\ s.seen[a] <= s.cursor+1
                  /\ s.cursor <= s.published /\ s.published <= Len(s.log)
NoInvocationAfterLocalCancel == [][Invoke => ~s.cancelled /\ ~s.consumerStopped]_s
ProducerUsesOwnConsumption == [][\A a \in Actors: Deliver(a) /\ s.requested' # s.requested => ~s.producerStopped]_s
=============================================================================
