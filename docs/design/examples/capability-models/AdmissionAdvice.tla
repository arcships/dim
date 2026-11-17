-------------------- MODULE AdmissionAdvice --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Decisions == {"allow","ask","deny","none"}
Init == s\in [own:{"allow","ask","deny"}, replies:[1..2->Decisions], active:SUBSET(1..2),
 accepted:{<<>>}, cursor:{1}, cancelled:{FALSE}, done:{FALSE}, result:{"none"}, advisorAsk:{FALSE}]
Withdraw(i) == s'=[s EXCEPT !.active=@\{i}]
Cancel == s'=[s EXCEPT !.cancelled=TRUE]
Advice == /\ ~s.done /\ ~s.cancelled /\ s.own#"deny" /\ s.cursor<=2
 /\ s'=[s EXCEPT !.accepted=Append(@,IF s.cursor\in s.active THEN s.replies[s.cursor] ELSE "none"), !.cursor=@+1]
Finish == /\ ~s.done /\ (s.cancelled \/ s.own="deny" \/ s.cursor=3)
 /\ LET vals=={s.accepted[i]:i\in 1..Len(s.accepted)} IN
 s'=[s EXCEPT !.done=TRUE, !.advisorAsk=("ask"\in vals),
 !.result=IF s.cancelled THEN "cancelled" ELSE IF s.own="deny" \/ "deny"\in vals THEN "deny"
 ELSE IF "ask"\in vals THEN "ask" ELSE IF "allow"\in vals THEN "allow" ELSE s.own]
Next == (\E i\in 1..2:Withdraw(i))\/Cancel\/Advice\/Finish
Spec == Init /\ [][Next]_s
OwnDenyWins == s.done /\ s.own="deny" => s.result\in{"deny","cancelled"}
AcceptedDenyWins == s.done /\ (\E i\in 1..Len(s.accepted):s.accepted[i]="deny") => s.result\in{"deny","cancelled"}
AskPreserved == s.done /\ s.result="ask" /\ s.own#"ask" => s.advisorAsk
NoAdviceOnDeny == s.own="deny" => Len(s.accepted)=0
NoAdviceAfterCancel == [][s.cancelled=>s.accepted'=s.accepted]_s
=============================================================================
