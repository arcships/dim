-------------------- MODULE InputQueue --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Ids == 1..3
Modes == {"queue","steer","append","handover"}
Init == s=[phase |-> [i \in Ids |-> "absent"], mode |-> [i \in Ids |-> "queue"],
 turn |-> FALSE, starting |-> FALSE, applied |-> {}, consumed |-> {}, deleted |-> {},
 replacements |-> {}, driven |-> FALSE]
Arrive(i,m) == /\ s.phase[i]="absent"
 /\ LET release == m\in{"steer","append"} \/ (m="queue" /\ ~s.turn /\ ~s.starting)
        drop == m="handover" /\ ~s.turn
 IN s'=[s EXCEPT !.mode[i]=m, !.phase[i]=IF drop THEN "deleted" ELSE IF release THEN "released" ELSE "held",
 !.applied=IF release THEN @\cup{i} ELSE @, !.deleted=IF drop THEN @\cup{i} ELSE @,
 !.starting= @ \/ (release /\ m#"append" /\ ~s.turn),
 !.driven= @ \/ (release /\ m#"append")]
Withdraw(i,replace) == /\ s.phase[i]="held"
 /\ s'=[s EXCEPT !.phase[i]="deleted", !.deleted=@\cup{i},
 !.replacements=IF replace THEN @\cup{i} ELSE @]
Take(i) == /\ s.phase[i]="released" \/ (s.phase[i]="held" /\ s.mode[i]="handover" /\ s.turn)
 /\ s'=[s EXCEPT !.phase[i]="consumed", !.consumed=@\cup{i}]
Start == /\ ~s.turn /\ s.starting /\ s'=[s EXCEPT !.turn=TRUE, !.starting=FALSE]
End == /\ s.turn
 /\ LET expired == {i\in Ids:s.phase[i]="held" /\ s.mode[i]="handover"}
        queued == {i\in Ids:s.phase[i]="held" /\ s.mode[i]="queue"}
        next == IF queued={} THEN 0 ELSE CHOOSE i\in queued: \A j\in queued:i<=j
 IN s'=[s EXCEPT !.turn=FALSE, !.starting=(next#0), !.driven=@\/(next#0),
 !.deleted=@\cup expired, !.applied=IF next=0 THEN @ ELSE @\cup{next},
 !.phase=[i\in Ids |-> IF i\in expired THEN "deleted" ELSE IF i=next THEN "released" ELSE s.phase[i]]]
Stop == s'=[s EXCEPT !.starting=FALSE]
Next == (\E i\in Ids,m\in Modes:Arrive(i,m)) \/ (\E i\in Ids,b\in BOOLEAN:Withdraw(i,b))
 \/ (\E i\in Ids:Take(i)) \/ Start \/ End \/ Stop
Spec == Init /\ [][Next]_s
NoWithdrawAfterHandoff == s.applied \cap s.deleted = {}
NoDeletedConsumption == s.consumed \cap s.deleted = {}
ReplacementOnlyDeleted == s.replacements \subseteq s.deleted
HandoverNotApplied == \A i\in s.applied:s.mode[i]#"handover"
NoAppendDriver == (\A i\in s.applied:s.mode[i]="append") => ~s.driven
NoDoubleMembership == \A i\in Ids: (i\in s.deleted <=> s.phase[i]="deleted") /\ (i\in s.consumed <=> s.phase[i]="consumed")
=============================================================================
