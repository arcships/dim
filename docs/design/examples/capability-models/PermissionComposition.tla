-------------------- MODULE PermissionComposition --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Decisions == {"allow","ask","deny","none"}
Base(p,r,a) == IF a \/ p="full" THEN "allow" ELSE
 IF r\in{"read","session"} \/ (p="workspace" /\ r="write") THEN "allow" ELSE IF p="readonly" THEN "deny" ELSE "ask"
Init == \E p\in{"full","readonly","workspace"},r\in{"read","write","execute","session","unknown"},a,d,c\in BOOLEAN,
 f\in{"workspace","outside","system","owned","grant","error","unverified","absent"},rule\in{"allow","deny","none","error"},adv\in SUBSET Decisions:
 s=[preset|->p,risk|->r,allowed|->a,disallowed|->d,compound|->c,fact|->f,rule|->rule,advisors|->adv,own|->"none",result|->"none"]
Own == /\ s.own="none"
 /\ LET b==Base(s.preset,s.risk,s.allowed) f==s.fact IN
 s'=[s EXCEPT !.own=CASE s.disallowed->"deny"
 [] s.risk="write" /\ (b="deny" \/ s.preset="readonly")->"deny"
 [] s.risk="write" /\ s.preset#"full"->CASE f\in{"system","owned","error"}->"deny" [] f\in{"workspace","grant"}->b [] OTHER->"ask"
 [] s.risk="read" /\ s.preset#"full" /\ f#"absent"->CASE f\in{"error","unverified"}->"deny" [] f="outside"->"ask" [] OTHER->b
 [] s.risk="execute"->CASE s.rule\in{"deny","error"}->"deny" [] b="ask" /\ s.rule="allow" /\ ~s.compound->"allow" [] OTHER->b
 [] OTHER->b]
Merge == /\ s.own#"none" /\ s.result="none"
 /\ s'=[s EXCEPT !.result=CASE s.own="deny" \/ "deny"\in s.advisors->"deny"
 [] "ask"\in s.advisors->"ask" [] "allow"\in s.advisors->"allow" [] OTHER->s.own]
Next == Own\/Merge
Spec == Init /\ [][Next]_s
DisallowedAlwaysDenies == s.disallowed /\ s.result#"none"=>s.result="deny"
ReadOnlyWriteDenies == s.preset="readonly" /\ s.risk="write" /\ s.result#"none"=>s.result="deny"
OwnDenyCannotBeOverridden == s.own="deny"=>s.result\in{"none","deny"}
AdvisorPriority == s.result#"none"=>IF "deny"\in s.advisors THEN s.result="deny" ELSE
 IF "ask"\in s.advisors /\ s.own#"deny" THEN s.result="ask" ELSE TRUE
CompoundAllowDoesNotBypassAsk == s.risk="execute" /\ s.compound /\ s.rule="allow" /\ Base(s.preset,s.risk,s.allowed)="ask"=>s.own\in{"none","ask","deny"}
ProtectedWriteGate == s.risk="write" /\ s.preset#"full" /\ s.fact\in{"system","owned","error"}=>s.own\in{"none","deny"}
=============================================================================
