-------------------- MODULE RetryBudget --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[attempt |-> 1, waited |-> 0, malformed |-> 0, credential |-> FALSE,
 irreversible |-> FALSE, cancelled |-> FALSE, stopped |-> FALSE, backoffs |-> 0,
 delays |-> <<>>, retryAttempts |-> <<>>]
Output == s'=[s EXCEPT !.irreversible=TRUE]
Cancel == s'=[s EXCEPT !.cancelled=TRUE, !.stopped=TRUE]
Nominal(n) == IF n<6 THEN 2^(n-1) ELSE 30
Failure(kind,hint) == /\ ~s.stopped
 /\ LET d==IF hint=0 THEN Nominal(s.backoffs+1) ELSE hint
        credential==kind="credential"
        retryable==kind\in{"transient","malformed","credential"}
        ok==~s.cancelled /\ ~s.irreversible /\ s.attempt<8 /\ retryable /\
         (IF credential THEN ~s.credential ELSE d<=300-s.waited /\ (kind#"malformed" \/ s.malformed=0))
 IN s'=[s EXCEPT !.stopped=~ok,
 !.attempt=IF ok THEN @+1 ELSE @,
 !.waited=IF ok /\ ~credential THEN @+d ELSE @,
 !.backoffs=IF ok /\ ~credential THEN @+1 ELSE @,
 !.malformed=IF ok /\ kind="malformed" THEN @+1 ELSE @,
 !.credential=@\/(ok /\ credential),
 !.delays=IF ok THEN Append(@,IF credential THEN 0 ELSE d) ELSE @,
 !.retryAttempts=IF ok THEN Append(@,[attempt|->s.attempt+1,safe|->~s.irreversible]) ELSE @]
Next == Output\/Cancel\/(\E k\in{"transient","malformed","credential","permanent","output_limit"},h\in{0,150,301}:Failure(k,h))
Spec == Init /\ [][Next]_s
AttemptBound == s.attempt<=8
WaitBound == s.waited<=300
MalformedBound == s.malformed<=1
ReplaySafe == \A i\in 1..Len(s.retryAttempts):s.retryAttempts[i].safe
CountMatchesRetries == s.attempt=Len(s.delays)+1
NoRetryAfterCancel == [][s.cancelled=>s.attempt'=s.attempt]_s
=============================================================================
