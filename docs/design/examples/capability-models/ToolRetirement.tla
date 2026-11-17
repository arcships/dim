-------------------- MODULE ToolRetirement --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Ids == 1..2
Available == s.live\cup{s.prefixPin}\cup{s.pins[i]:i\in Ids}
Init == \E held\in 0..1:s=[live|->{1},configured|->1,prefixPin|->held,pins|->[i\in Ids|->0],declared|->[i\in Ids|->0],results|->[i\in Ids|->"none"], ended|->{},attempted|->{},ran|->[i\in Ids|->0],faceActive|->TRUE,faceReply|->"none"]
Replace == s'=[s EXCEPT !.live=(@\{1})\cup{2}]
Adopt == /\ 2\in s.live /\ s'=[s EXCEPT !.configured=2, !.prefixPin=2]
Bind(i) == /\ i\notin s.attempted /\ s'=[s EXCEPT !.attempted=@\cup{i}, !.results[i]=IF s.configured\in Available THEN "bound" ELSE "sourceUnavailable", !.pins[i]=IF s.configured\in Available THEN s.configured ELSE 0, !.declared[i]=s.configured]
Run(i,ok) == /\ s.pins[i]#0 /\ i\notin s.ended /\ s'=[s EXCEPT !.ran[i]=s.pins[i], !.results[i]=IF ok THEN "completed" ELSE "handlerError"]
End(i) == /\ i\in s.attempted /\ s'=[s EXCEPT !.pins[i]=0, !.ended=@\cup{i}]
RetireFace == s'=[s EXCEPT !.faceActive=FALSE]
CallFace == s'=[s EXCEPT !.faceReply=IF s.faceActive THEN "entered" ELSE "unavailable"]
Next == Replace\/Adopt\/(\E i\in Ids:Bind(i)\/End(i))\/(\E i\in Ids,b\in BOOLEAN:Run(i,b))\/RetireFace\/CallFace
Spec == Init /\ [][Next]_s
OldRequestNeverRunsReplacement == \A i\in Ids:s.ran[i]#0=>s.ran[i]=s.declared[i]
UnavailableDoesNotBind == \A i\in Ids:s.results[i]="sourceUnavailable"=>s.pins[i]=0 /\ s.ran[i]=0
RetirementDoesNotReleaseRequestPins == [][Replace=>s.pins'=s.pins]_s
RetiredFaceCannotEnter == [][CallFace /\ ~s.faceActive=>s.faceReply'="unavailable"]_s
StillHeldVersionCanBind == [][\A i\in Ids:Bind(i) /\ s.configured\in Available=>s.results'[i]="bound"]_s
=============================================================================
