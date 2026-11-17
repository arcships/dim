-------------------- MODULE HookReplyMeaning --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == \E point\in{"prompt","preTool","permission"},outcome\in{"success","blocked","failed"},block\in BOOLEAN, specific\in{"allow","ask","deny","defer","none"},legacy\in{"block","approve","none"},behavior\in{"allow","deny","none"}: s=[point|->point,outcome|->outcome,block|->block,specific|->specific,legacy|->legacy,behavior|->behavior,result|->"pending"]
Decide == /\ s.result="pending" /\ s'=[s EXCEPT !.result=CASE s.point="permission"->CASE s.outcome="blocked"->"none" [] s.outcome="failed"->IF s.block THEN "deny" ELSE "none" [] OTHER->s.behavior [] s.point="preTool"->CASE s.outcome="blocked" \/ (s.outcome="failed" /\ s.block)->"deny" [] s.outcome="failed"->"none" [] s.specific\in{"allow","ask","deny"}->s.specific [] s.specific="defer"->"none" [] s.legacy="block"->"deny" [] s.legacy="approve"->"allow" [] OTHER->"none" [] OTHER->CASE s.outcome="blocked" \/ (s.outcome="failed" /\ s.block) \/ (s.outcome="success" /\ s.legacy="block")->"block" [] s.outcome="failed"->"unfinished" [] OTHER->"contextOrNone"]
Next == Decide
Spec == Init /\ [][Next]_s
PermissionExitTwoHasNoOpinion == s.point="permission" /\ s.outcome="blocked"=>s.result\in{"pending","none"}
PreToolSpecificWinsLegacy == s.point="preTool" /\ s.outcome="success" /\ s.specific\in{"allow","ask","deny"}=>s.result\in{"pending",s.specific}
ExplicitDeferCannotLegacyBlock == s.point="preTool" /\ s.outcome="success" /\ s.specific="defer"=>s.result\in{"pending","none"}
FailedPromptContinueReportsUnfinished == s.point="prompt" /\ s.outcome="failed" /\ ~s.block=>s.result\in{"pending","unfinished"}
=============================================================================
