-------------------- MODULE UsageIdentity --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
VARIABLE s
Init == s=[epoch |-> 0, algorithm |-> 0, request |-> 0, measured |-> 0,
 savedEpoch |-> 0, savedAlgorithm |-> 0, saved |-> FALSE, available |-> FALSE,
 pin |-> 0, weakAlive |-> FALSE, calibration |-> 0, calibratedFor |-> 0, calibratedEpoch |-> 0]
Prepare == /\ s.request<2
 /\ s'=[s EXCEPT !.request=@+1, !.pin=s.request+1, !.weakAlive=TRUE, !.calibration=0,
 !.saved=TRUE, !.savedEpoch=s.epoch, !.savedAlgorithm=s.algorithm, !.available=TRUE]
DropRequest == s'=[s EXCEPT !.weakAlive=FALSE, !.calibration=0]
NewEpoch == /\ s.epoch=0 /\ s'=[s EXCEPT !.epoch=1, !.available=FALSE, !.calibration=0, !.weakAlive=FALSE]
ChangeAlgorithm == /\ s.algorithm=0 /\ s'=[s EXCEPT !.algorithm=1, !.available=FALSE, !.calibration=0, !.weakAlive=FALSE]
Measure(id) == /\ s.weakAlive /\ id=s.pin /\ s.available
 /\ s'=[s EXCEPT !.measured=id, !.calibration=1, !.calibratedFor=id, !.calibratedEpoch=s.epoch]
Recover(schemaOK) == s'=[s EXCEPT !.weakAlive=FALSE, !.calibration=0,
 !.available=schemaOK /\ s.saved /\ s.savedEpoch=s.epoch /\ s.savedAlgorithm=s.algorithm]
Next == Prepare\/DropRequest\/NewEpoch\/ChangeAlgorithm\/(\E i\in 0..2:Measure(i))\/(\E b\in BOOLEAN:Recover(b))
Spec == Init /\ [][Next]_s
CompatibleResult == s.available => s.savedEpoch=s.epoch /\ s.savedAlgorithm=s.algorithm
CalibrationHasLiveIdentity == s.calibration#0 => s.weakAlive /\ s.calibratedFor=s.pin /\ s.calibratedEpoch=s.epoch
NoCalibrationRestored == [][\A b\in BOOLEAN:Recover(b) => s.calibration'=0]_s
=============================================================================
