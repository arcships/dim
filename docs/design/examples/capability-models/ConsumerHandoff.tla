-------------------- MODULE ConsumerHandoff --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
CONSTANTS Consumers, Events
VARIABLE s
Ids == 1..Consumers
Matches(c,e) == c = 2 \/ s.keys[e] = "a/x" \/ (c = 3 /\ s.keys[e] = "a/y")
RECURSIVE Suffix(_,_,_)
Suffix(c,lo,hi) == IF lo > hi THEN <<>> ELSE
    (IF Matches(c,lo) THEN <<lo>> ELSE <<>>) \o Suffix(c,lo+1,hi)
Empty == [active |-> FALSE, phase |-> "absent", cut |-> 0, captured |-> FALSE,
          held |-> <<>>, batch |-> <<>>, busy |-> 0, source |-> "none",
          calls |-> <<>>, ready |-> FALSE]
Init == \E keys \in [1..Events -> {"a/x","a/y","b/z"}]:
    s = [keys |-> keys, allocated |-> 0, published |-> 0, cursor |-> 0,
         current |-> 0, todo |-> {}, busBusy |-> 0, fault |-> "none",
         faultAt |-> 0, faultOwner |-> 0, aborted |-> FALSE,
         consumers |-> [c \in Ids |-> Empty]]
Write == /\ s.allocated < Events /\ s' = [s EXCEPT !.allocated = @+1]
Publish == /\ s.published < s.allocated /\ s' = [s EXCEPT !.published = @+1]
Register(c) ==
    /\ s.consumers[c].phase = "absent"
    /\ s' = [s EXCEPT !.consumers[c].active = TRUE, !.consumers[c].phase = "rebuild"]
Capture(c,cut) ==
    /\ s.consumers[c].phase = "rebuild" /\ ~s.consumers[c].captured
    /\ cut \in s.published..s.allocated
    /\ s' = [s EXCEPT !.consumers[c].cut = cut, !.consumers[c].captured = TRUE]
Rebuilt(c,ok) ==
    /\ s.consumers[c].phase = "rebuild" /\ s.consumers[c].captured
    /\ s' = [s EXCEPT !.consumers[c].phase = IF ok THEN "drain" ELSE "failed",
                     !.consumers[c].ready = ok]
Take(c) ==
    /\ s.consumers[c].phase = "drain"
    /\ s.consumers[c].batch = <<>> /\ s.consumers[c].busy = 0
    /\ s' = [s EXCEPT !.consumers[c].batch = @ \o s.consumers[c].held,
                     !.consumers[c].held = <<>>,
                     !.consumers[c].phase = IF s.consumers[c].held = <<>> THEN "live" ELSE @]
Drain(c) ==
    /\ s.consumers[c].phase = "drain" /\ s.consumers[c].busy = 0
    /\ s.consumers[c].batch # <<>>
    /\ LET e == Head(s.consumers[c].batch) covered == e <= s.consumers[c].cut
       IN s' = [s EXCEPT !.consumers[c].batch = Tail(@),
                        !.consumers[c].busy = IF covered THEN 0 ELSE e,
                        !.consumers[c].source = IF covered THEN "none" ELSE "drain",
                        !.consumers[c].calls = IF covered THEN @ ELSE Append(@,e)]
Begin ==
    /\ s.current = 0 /\ s.fault = "none" /\ s.cursor < s.published
    /\ s' = [s EXCEPT !.current = s.cursor+1,
                     !.todo = {c \in Ids:s.consumers[c].active}]
Route(c) ==
    /\ s.current # 0 /\ s.busBusy = 0 /\ c \in s.todo /\ ~s.aborted
    /\ LET owner == s.consumers[c]
           selected == owner.active /\ Matches(c,s.current)
           holding == owner.phase # "live"
           invoke == selected /\ ~holding /\ s.current > owner.cut
       IN s' = [s EXCEPT !.todo = @ \ {c},
                        !.busBusy = IF invoke THEN c ELSE 0,
                        !.consumers[c].held = IF selected /\ holding THEN Append(@,s.current) ELSE @,
                        !.consumers[c].busy = IF invoke THEN s.current ELSE @,
                        !.consumers[c].source = IF invoke THEN "live" ELSE @,
                        !.consumers[c].calls = IF invoke THEN Append(@,s.current) ELSE @]
Finish(c,outcome) ==
    /\ s.consumers[c].busy # 0
    /\ outcome # "cancel" \/ ~s.consumers[c].active
    /\ LET live == s.consumers[c].source = "live"
           failed == outcome \in {"error","panic"}
           first == live /\ failed /\ s.fault = "none"
       IN s' = [s EXCEPT !.consumers[c].busy = 0, !.consumers[c].source = "none",
                        !.consumers[c].phase = IF ~live /\ (failed \/ outcome = "cancel") THEN "failed" ELSE @,
                        !.busBusy = IF live THEN 0 ELSE @,
                        !.fault = IF first THEN outcome ELSE @,
                        !.faultAt = IF first THEN s.current ELSE @,
                        !.faultOwner = IF first THEN c ELSE @,
                        !.aborted = @ \/ (live /\ outcome = "panic")]
EndEvent ==
    /\ s.current # 0 /\ s.busBusy = 0 /\ (s.todo = {} \/ s.aborted)
    /\ s' = [s EXCEPT !.cursor = IF s.aborted THEN @ ELSE s.current,
                     !.current = 0, !.todo = {}]
Remove(c) ==
    /\ s.consumers[c].active
    /\ s' = [s EXCEPT !.consumers[c].active = FALSE]
DropRebuild(c) ==
    /\ ~s.consumers[c].active /\ s.consumers[c].phase \in {"rebuild","drain"}
    /\ s.consumers[c].busy = 0
    /\ s' = [s EXCEPT !.consumers[c].phase = "failed"]
Next == Write \/ Publish \/ Begin \/ EndEvent
    \/ (\E c \in Ids:Register(c) \/ Take(c) \/ Drain(c) \/ Route(c) \/ Remove(c) \/ DropRebuild(c)
        \/ (\E cut \in 0..Events:Capture(c,cut))
        \/ (\E ok \in BOOLEAN:Rebuilt(c,ok))
        \/ (\E outcome \in {"ok","error","panic","cancel"}:Finish(c,outcome)))
Spec == Init /\ [][Next]_s
OrderedOnce == \A c \in Ids: \A i \in 1..Len(s.consumers[c].calls):
    /\ s.consumers[c].calls[i] > s.consumers[c].cut
    /\ Matches(c,s.consumers[c].calls[i])
    /\ (i = 1 \/ s.consumers[c].calls[i-1] < s.consumers[c].calls[i])
NoHandlerBeforeRebuild == \A c \in Ids: s.consumers[c].calls # <<>> => s.consumers[c].ready
ExactLivePrefix == \A c \in Ids:
    s.consumers[c].active /\ s.consumers[c].phase = "live" /\ s.current = 0 /\ s.fault = "none"
    => s.consumers[c].calls = Suffix(c,s.consumers[c].cut+1,s.cursor)
NoFutureEventAfterFault == s.fault # "none" =>
    s.cursor <= s.faultAt /\ s.current <= s.faultAt
RetainsFirstFault == [][s.fault # "none" =>
    s.fault' = s.fault /\ s.faultOwner' = s.faultOwner /\ s.faultAt' = s.faultAt]_s
RemovalPreservesHistory == [][\A c \in Ids:Remove(c) =>
    s.allocated' = s.allocated /\ s.published' = s.published /\ s.keys' = s.keys]_s
=============================================================================
