-------------------- MODULE GoalLifecycle --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
States == {"none","active","paused","completed","blocked"}
Init == s=[durable |-> "none", memory |-> "none", running |-> FALSE, generation |-> 0,
 commands |-> 0, starts |-> 0, afterReload |-> FALSE, continuations |-> 0, liveObjects |-> {}]
SetOrResume == /\ s.commands<2
 /\ s'=[s EXCEPT !.durable="active", !.memory="active", !.running=TRUE,
 !.commands=@+1, !.starts=@+1, !.afterReload=FALSE, !.continuations=0, !.liveObjects={}]
Business(v) == /\ v#"active" /\ s'=[s EXCEPT !.durable=v, !.memory=v, !.running=FALSE, !.liveObjects={}]
Signal(i) == /\ s.running /\ s.memory="active" /\ s.continuations<2
 /\ s'=[s EXCEPT !.continuations=@+1, !.liveObjects=@\cup{i}]
Reload == /\ s.generation<2
 /\ s'=[s EXCEPT !.generation=@+1, !.memory=IF s.durable="active" THEN "paused" ELSE s.durable,
 !.running=FALSE, !.afterReload=TRUE, !.continuations=0, !.liveObjects={}]
Next == SetOrResume\/(\E v\in States:Business(v))\/(\E i\in 1..2:Signal(i))\/Reload
Spec == Init /\ [][Next]_s
ExplicitStartOnly == s.starts=s.commands
ReloadCannotRun == s.afterReload => ~s.running /\ s.memory#"active"
NoRuntimeRecovery == s.afterReload => s.liveObjects={} /\ s.continuations=0
RunningMeansActive == s.running => s.memory="active"
NoSyntheticPauseWrite == [][s.generation'>s.generation => s.durable'=s.durable]_s
=============================================================================
