--------------------- MODULE BranchCapSwitch ---------------------
EXTENDS Naturals, FiniteSets, TLC
\* The Session's cap over the whole tree, on the one switch a single tracked
\* line cannot show: a nested pair that starts counting together.
\*
\* Two open tool lines, A (1) and B (2) nested below A, and the Session's
\* conversation somewhere in that subtree. While the role is below B both are
\* on the protected path and count nothing. When the role returns to the root,
\* both leave that path at the same moment.
\*
\* The count is §6.1's set difference, applied to the new ancestry of every
\* open tool line rather than to the line that gained or lost the role:
\* `Counted` is the open tool lines minus the protected path, so a switch
\* recomputes it for every line at once. No step closes a line, so a Session a
\* switch leaves over the cap stays over it and only a later open is refused.
VARIABLE role, created
Cap == 2
A == 1
B == 2
New == {3, 4}
BelowB == "belowB"
BelowAOnly == "betweenAAndB"
AtRoot == "root"
Positions == {BelowB, BelowAOnly, AtRoot}
\* Every open tool line of the Session: the two nested ones plus whatever an
\* open added. No transition removes one.
OpenLines == {A, B} \cup created
\* The protected path: the default branch and each of its ancestors. Below B
\* both lines are on it; between A and B only A is; on the root neither is.
Protected == CASE role = AtRoot -> {}
                  [] role = BelowAOnly -> {A}
                  [] OTHER -> {A, B}
Counted == OpenLines \ Protected
Total == Cardinality(Counted)
Room == Cap - Total
CanCreate == Room > 0 /\ created /= New
\* A switch is the tree's own fact: the user moves the Session's conversation
\* and no count can refuse it. What it does change is which lines are
\* protected, and this step recomputes the set for all of them at once.
SwitchTo(p) == /\ role' = p
               /\ UNCHANGED created
Create == /\ CanCreate
          /\ \E n \in (New \ created): created' = created \cup {n}
          /\ UNCHANGED role
Next == Create \/ SwitchTo(BelowB) \/ SwitchTo(BelowAOnly) \/ SwitchTo(AtRoot)
\* A Session starts inside the cap. An over-cap total is what a switch leaves,
\* never a state an open can produce.
Init == \E p \in Positions, c \in SUBSET New:
  /\ role = p
  /\ created = c
  /\ Cardinality(Counted) <= Cap
Spec == Init /\ [][Next]_<<role, created>>
\* An open is admitted only with room and spends one slot, so it never takes
\* the Session past its cap.
CreateStaysWithinCap == [][Create => Total' <= Cap]_<<role, created>>
\* A switch closes nothing: the open tool lines before it are the open tool
\* lines after it.
SwitchClosesNothing == [][role' /= role => created' = created]_<<role, created>>
\* The scenario the review named, as its own specification: the cap is two,
\* A and B are the only open tool lines, and the role sits below both. The
\* switch back to the root is the only step, so the state after it is the one
\* the tree reaches: both lines count, the total reaches the cap exactly, and
\* no open is left.
BackToRootInit == role = BelowB /\ created = {}
BackToRootSpec == BackToRootInit /\ [][SwitchTo(AtRoot)]_<<role, created>>
  /\ WF_<<role, created>>(SwitchTo(AtRoot))
TheSwitchIsAdmitted == <>(role = AtRoot)
BothNestedLinesStartCounting == [][role = AtRoot => A \in Counted /\ B \in Counted]_<<role, created>>
TheSwitchSpendsEverySlot == [][role = AtRoot => Total = Cap]_<<role, created>>
TheSwitchLeavesNoRoom == [][role = AtRoot => Room = 0]_<<role, created>>
NoOpenAfterTheSwitch == [][role = AtRoot => ~CanCreate]_<<role, created>>
\* The same switch with one line opened meanwhile: both nested lines count and
\* so does the third, so the Session is left over its cap and still nothing is
\* closed.
OverCapInit == role = BelowB /\ created = {3}
OverCapSpec == OverCapInit /\ [][SwitchTo(AtRoot)]_<<role, created>>
  /\ WF_<<role, created>>(SwitchTo(AtRoot))
OverCapAfterTheSwitch == [][role = AtRoot => Total > Cap]_<<role, created>>
======================================================================
