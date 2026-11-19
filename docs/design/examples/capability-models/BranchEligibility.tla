-------------------- MODULE BranchEligibility --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
\* One line's eligibility, in a Session whose default branch is a position in
\* the tree. Nothing here is a role marker: a line is the root when it has no
\* parent, and its opener is that parent, so both are read from the tree
\* (`IsRoot`, `HasOpener`). `primary` says the line is the Session's default
\* branch: the line its conversation is on. `source` says where the line was
\* created from, and only that.
\*
\* This model checks what a line may do, never when it should do it: opening
\* policy is not modelled. What it does fix is who may open a line. Two lines
\* qualify, the root always and the current default branch while it holds the
\* role (`CanOpen`), so at most two lines in a Session can open one. That is
\* what keeps the root able to answer a user who asks it for a line while
\* another line holds the role: a guard that let only the default branch open
\* one would refuse that request.
\*
\* `ancestorEnding` and `descendantDefault` are the tree facts the admission
\* rules read: some ancestor's end is already on its way down, which the
\* cascade will complete, and some descendant is the default branch. The
\* environment sets each of them, and that is how the two conflicting orderings
\* reach this line: an ancestor's end can commit first, or a descendant can
\* become the default branch first. Exactly one of the two is admitted.
\*
\* The cap counts the open tool lines of the Session that are not on the
\* protected path, which is the default branch and its ancestors, because a
\* line on that path cannot be closed. `Counts` says whether this line is one
\* of them, `Counted` is the slot it spends, and `peers` counts the others, so
\* the Session's room is `Cap - Counted - peers` for an open from any line. A new child of any line
\* is off that path: it is neither the default branch, which a move sets on a
\* line that already exists, nor an ancestor of it, because it is new and below
\* this one. So an open spends a slot, and the room changes only when the role
\* moves: onto this line, which stops it counting; off it, which starts it
\* counting again; or away from below this line, which leaves this line off the
\* protected path too and starts it counting again (`DefaultLeavesBelow`).
VARIABLE s
NoParent == "none"
Cap == 2
\* The status note under `branch:line` that BranchLine wrote on this line: none
\* yet, the one that says the line is the default branch, or the one that says
\* it no longer is. It is a keyed block, so its latest value replaces the
\* earlier one; what it must never do is reach the root.
NoNote == "none"
DefaultNote == "default"
NotDefaultNote == "notDefault"
\* A line the user may choose as the default branch: the root, a tool line and a
\* line the user opened. A plugin line follows its creator's protocol, and a
\* line a client created before the upgrade keeps its stored `host` row, so
\* neither is selectable.
Selectable == {"tool","user"}
IsRoot == s.parent = NoParent
HasOpener == ~IsRoot
\* The Session's two openers, and the whole rule: the root in every state, and
\* the default branch while it is one. A plugin line is never either of them,
\* because it is never the default branch and is never the root.
CanOpen == IsRoot \/ (s.primary /\ s.source \in Selectable)
\* The root is a target whenever it does not hold the role: a move away from it
\* leaves it open and no longer the default branch, and nothing else about it
\* changes, so the user can always move back to where the Session started.
CanBeMovedTo == ~s.primary /\ (IsRoot \/ s.source\in Selectable) /\ ~s.ended /\ ~s.ending /\ ~s.ancestorEnding
\* Only a tool line ends itself: a line the user opened is ended by the user, a
\* plugin line by its creator's protocol, and the root never ends. A line that
\* is the default branch, or that has a descendant which is, is refused too,
\* because ending a line ends every line below it.
CanEndSelf == HasOpener /\ ~s.primary /\ ~s.descendantDefault /\ ~s.ancestorEnding /\ s.source="tool"
\* An end asked for from outside the line: the user's, a creator's or a
\* cascade's. The end entry checks the target only, whoever asks.
CanBeEnded == HasOpener /\ ~s.primary /\ ~s.descendantDefault /\ ~s.ended /\ ~s.ending
\* This line is on the protected path when it is the default branch or some
\* descendant of it is, and a line opened with the tool counts against the cap
\* while it is open and off that path.
Counts == s.source="tool" /\ ~s.primary /\ ~s.descendantDefault
\* This line's own contribution to the Session's counted total, and that total:
\* its own slot plus the slots its peers hold.
Counted == IF Counts THEN 1 ELSE 0
CountedTotal == Counted + s.peers
\* An open from this line: it must be allowed to open one, the line is not
\* ending, the Session must have a slot, and this line holds fewer than two
\* children, which is the model's bound and not a rule. The child is off the
\* protected path, so the open spends a slot however the caller is placed.
RoomLeft == Cap - Counted - s.peers
CanCreate == CanOpen /\ ~s.ending /\ ~s.ended /\ RoomLeft>0 /\ Cardinality(s.children)<2
\* A line that has an opener, which is every line but the root. The root is
\* `RootInit` alone: a parentless tool, user, plugin or upgraded line is not a
\* state the tree can hold, and starting one here would not exercise the root.
\* A new line is never the default branch and holds no note: it becomes one by
\* a move, which is where the note is written. It counts against the cap unless
\* it is a user or plugin line, so `peers` starts inside the cap this line's own
\* membership leaves.
Init == \E source\in{"tool","user","plugin","hostOld"},notice\in BOOLEAN,peers\in 0..Cap:
 /\ (source="tool" => peers<Cap)
 /\ s=[parent|->"P1",source|->source,primary|->FALSE,note|->NoNote,notice|->notice,peers|->peers,
     head|->1,opening|->1,cut|->0,cursor|->0,reported|->1,held|->FALSE,
     ended|->FALSE,ending|->FALSE,turn|->FALSE,steered|->FALSE,ownEnd|->FALSE,pending|->FALSE,recovery|->FALSE,idle|->0,
     blocks|-><<>>,roster|->{1,2},endedNotice|->{},externalEnds|->{},
     ancestorEnding|->FALSE,descendantDefault|->FALSE,children|->{}]
\* The Session starts on its root, so the root is the default branch here. That
\* it later holds no role is a reachable state, not an initial one:
\* `AnotherLineTakesRole` leaves it open and no longer the default branch, which
\* is the state in which it is a target again.
RootInit == \E notice\in BOOLEAN,peers\in 0..Cap:
 /\ s=[parent|->NoParent,source|->"root",primary|->TRUE,note|->NoNote,notice|->notice,peers|->peers,
     head|->1,opening|->1,cut|->0,cursor|->0,reported|->1,held|->FALSE,
     ended|->FALSE,ending|->FALSE,turn|->FALSE,steered|->FALSE,ownEnd|->FALSE,pending|->FALSE,recovery|->FALSE,idle|->0,
     blocks|-><<>>,roster|->{1,2},endedNotice|->{},externalEnds|->{},
     ancestorEnding|->FALSE,descendantDefault|->FALSE,children|->{}]
\* One line's state, for the specifications that start from a named state
\* instead of exploring every line the tree can hold.
LineState(source,primary,descendantDefault,peers) ==
  [parent|->"P1",source|->source,primary|->primary,note|->NoNote,notice|->FALSE,peers|->peers,
   head|->1,opening|->1,cut|->0,cursor|->0,reported|->1,held|->FALSE,
   ended|->FALSE,ending|->FALSE,turn|->FALSE,steered|->FALSE,ownEnd|->FALSE,pending|->FALSE,recovery|->FALSE,idle|->0,
   blocks|-><<>>,roster|->{1,2},endedNotice|->{},externalEnds|->{},
   ancestorEnding|->FALSE,descendantDefault|->descendantDefault,children|->{}]
\* Becoming the default branch. A target whose ancestor is already ending is
\* refused, because the cascade will reach it, and so is the line that already
\* holds the position, whatever line it is: the root is refused while it is the
\* default branch and accepted once another line holds the role. The role is one
\* position, so a line that takes it has nothing below it holding the role: a
\* default descendant is the line that now loses it. A move writes one status
\* note, under the identity's own key, to the line that gains the role and to
\* the one that loses it; the root gets none in either direction, because its
\* two rules, that it opens lines and that it never ends, are the same whether
\* or not it holds the role. A move closes no line, so the room it changes is
\* the room its own placement changes.
Move == /\ CanBeMovedTo
 /\ s'=[s EXCEPT !.primary=TRUE, !.descendantDefault=FALSE, !.note=IF IsRoot THEN @ ELSE DefaultNote]
\* Another line becomes the default branch, so this one stops holding the role.
\* The move is that line's own action and its own note; this line records only
\* what it loses, and the root records nothing, as when it gains the role. This
\* is the route by which a root that no longer holds the role is reached; from
\* there `Move` takes it back. Losing the role puts an open tool line back on
\* the counted side, whether or not the Session still has room: nothing closes
\* a line to make room, and a later open is what the cap refuses.
AnotherLineTakesRole == /\ s.primary
 /\ s'=[s EXCEPT !.primary=FALSE, !.note=IF IsRoot THEN @ ELSE NotDefaultNote]
\* The two tree facts. Each can arrive at any time, and only the admission
\* rules decide what follows from it.
\* Opening a line. The child is a new open tool line off the protected path, so
\* it spends one slot whichever line opened it, and the Session's other counted
\* lines stay where they were.
Create == /\ CanCreate
 /\ s'=[s EXCEPT !.children=@\cup{Cardinality(s.children)+1}, !.peers=@+1]
\* The root has no ancestor, so no ancestor's end can be on its way to it.
BeginAncestorEnd == /\ HasOpener /\ ~s.ancestorEnding /\ ~s.ended /\ s'=[s EXCEPT !.ancestorEnding=TRUE]
BeginDescendantDefault == /\ ~s.descendantDefault /\ ~s.primary /\ ~s.ended
 /\ s'=[s EXCEPT !.descendantDefault=TRUE]
\* The role leaves this line's subtree: the line that held it is no longer the
\* default branch, so this line is off the protected path again and counts again
\* when it was opened with the tool, whether or not the Session still has room.
\* Nothing closes a line to make room, and a later open is what the cap refuses.
\* This is the tree fact a switch back to the root leaves on every tool ancestor
\* of the line the conversation moved away from.
DefaultLeavesBelow == /\ s.descendantDefault /\ ~s.primary /\ ~s.ended
 /\ s'=[s EXCEPT !.descendantDefault=FALSE]
ParentAdvance == /\ s.head<2 /\ s'=[s EXCEPT !.head=@+1]
Read(after,through,ok) == /\ after<=s.head /\ through<=s.head /\ through>=after
 /\ LET floor==IF IsRoot THEN after ELSE IF after<s.cut THEN s.cut ELSE after
 IN /\ through>=floor /\ s'=[s EXCEPT !.cursor=IF ok THEN through ELSE @]
Block == /\ HasOpener /\ ~s.ended /\ ~s.held /\ Len(s.blocks)<1
 /\ s.head>s.opening /\ s.head>s.cursor /\ s.head>s.reported
 /\ s'=[s EXCEPT !.held=TRUE, !.reported=s.head, !.blocks=Append(@,[head|->s.head,cursor|->s.cursor,opening|->s.opening])]
ConsumeBlock == s'=[s EXCEPT !.held=FALSE]
Start == /\ ~s.turn /\ ~s.ended /\ ~s.recovery /\ s.idle<1
 /\ s'=[s EXCEPT !.turn=TRUE, !.steered=FALSE, !.ownEnd=FALSE]
Steer(ok) == /\ s.turn /\ s'=[s EXCEPT !.steered=@\/ok]
Pending(b) == s'=[s EXCEPT !.pending=b]
OwnEnd == /\ s.turn /\ CanEndSelf /\ s'=[s EXCEPT !.ownEnd=TRUE, !.ending=TRUE]
EndFromOutside == /\ CanBeEnded /\ s'=[s EXCEPT !.ending=TRUE, !.turn=FALSE]
FinishEnd == /\ s.ending /\ s'=[s EXCEPT !.ending=FALSE, !.ended=TRUE]
EndTurn == /\ s.turn /\ s.idle<1
 /\ s'=[s EXCEPT !.turn=FALSE,
     !.idle=IF s.source="tool" /\ ~s.primary /\ s.notice /\ ~s.steered /\ ~s.ownEnd /\ ~s.pending THEN @+1 ELSE @]
Restore == /\ ~s.recovery /\ s'=[s EXCEPT !.recovery=TRUE, !.turn=FALSE]
\* The opener ends a line it opened, and the roster is the opener's own. The
\* default branch may end the lines it opened; no line ever ends itself this way.
ChildEnd(i,own) == /\ HasOpener /\ i\in s.roster
 /\ s'=[s EXCEPT !.roster=@\{i}, !.endedNotice=IF own THEN @ ELSE @\cup{i}, !.externalEnds=IF own THEN @ ELSE @\cup{i}]
Next == Move\/AnotherLineTakesRole\/Create\/BeginAncestorEnd\/BeginDescendantDefault\/DefaultLeavesBelow\/ParentAdvance
 \/(\E a,t\in 0..2,b\in BOOLEAN:Read(a,t,b))\/Block\/ConsumeBlock\/Start
 \/(\E b\in BOOLEAN:Steer(b)\/Pending(b))\/OwnEnd\/EndFromOutside\/FinishEnd\/EndTurn\/Restore
 \/(\E i\in 1..2,b\in BOOLEAN:ChildEnd(i,b))
Spec == (Init\/RootInit) /\ [][Next]_s
ParentBlockHasNewActivity == \A n\in 1..Len(s.blocks):s.blocks[n].head>s.blocks[n].cursor /\ s.blocks[n].head>s.blocks[n].opening
\* The root has nothing above it: a line with no opener gets no parent block.
NoRootParentBlock == IsRoot=>s.blocks= <<>>
EndNoticeAndRosterAtomic == s.endedNotice=s.externalEnds /\ s.endedNotice\cap s.roster={}
IdleEligibility == [][s.idle'>s.idle=>s.source="tool" /\ ~s.primary /\ s.notice /\ ~s.steered /\ ~s.ownEnd /\ ~s.pending /\ s.turn]_s
RestoreNoOldIdle == [][Restore=>s.idle'=s.idle]_s
ExplicitReadCanReset == [][\A a,t\in 0..2:Read(a,t,TRUE)=>s.cursor'=t]_s
\* A line never ends itself while it is the root or the default branch. Out of
\* scope here: an end cascaded from an ancestor, which the ancestor check on the
\* end entry guards, not this model.
RootAndPrimaryEndNotThemselves == (IsRoot\/s.primary)=>~s.ended
\* A line the user opened is never ended by its own `end`: the user ends it.
UserLineNeverEndsItself == s.source="user"=>~s.ownEnd
\* A plugin line is never the default branch, and neither is a line a client
\* created before the upgrade: nothing rewrites either kind.
PluginNeverDefault == s.source="plugin"=>~s.primary
UpgradedHostLineNeverDefault == s.source="hostOld"=>~s.primary
\* The default branch is an open conversation line.
DefaultIsOpenConversationLine == s.primary=>((IsRoot \/ s.source\in Selectable) /\ ~s.ended /\ ~s.ending)
\* Who may open a line: the root always, and the default branch while it holds
\* the role. Every other line, a former default branch included, is refused, so
\* at most two lines in a Session qualify and the root never loses the right.
OnlyRootAndDefaultOpen == [][Create=>s.primary \/ IsRoot]_s
RootAlwaysCanOpen == [][IsRoot=>CanOpen]_s
\* Gaining the role writes one status note to a line that has an opener. The root
\* is the exception the next invariant covers: its two rules do not change with
\* the role.
MoveWritesOneNote == [][Move /\ ~IsRoot=>s.note'=DefaultNote]_s
\* A move changes no roster: the lines a line opened stay its own, because
\* coordination follows the tree, not the default branch.
MoveKeepsRoster == [][Move=>s.roster'=s.roster]_s
\* The root receives no note, whichever direction the role moves: it opens lines
\* and never ends whether or not it is the default branch. Because the note is a
\* keyed block and the root is never written one, the root's key holds the
\* roster alone, which is what a later compaction of the root replays.
RootGetsNoNote == IsRoot=>s.note=NoNote
\* A root that another line has taken the role from is a valid move target, so
\* the return to where the Session started is always admitted. `RootInit` starts
\* the Session on its root and `AnotherLineTakesRole` is the reachable state this
\* reads, so the root's own case is the one checked, not a parentless tool line.
ReturnToRootAdmitted == IsRoot /\ ~s.primary=>CanBeMovedTo
\* The role is one position, so a line that holds it has no descendant that
\* does: a move takes it from wherever it was, and the tree facts below never
\* add one under a line that already holds it.
OneRolePosition == ~(s.primary /\ s.descendantDefault)
\* The cap refuses a new open and nothing else. An open is admitted only while
\* the Session has room, and it spends exactly one, because the line it opens
\* is off the protected path however the opener is placed. A move closes no
\* line: it only decides whether this line counts, so a move that puts a line
\* back on the counted side can leave the Session over the cap with no room
\* left for a later open.
CapOnlyRefusesNewOpens == [][Create=>RoomLeft>0]_s
\* An open leaves the Session at most at its cap. It is admitted only with room,
\* and it spends exactly one, so the cap never refuses what it has admitted; a
\* later switch is what can put the total above the cap.
CreateLeavesAtMostTheCap == [][Create=>CountedTotal'<=Cap]_s
ANewChildSpendsASlot == [][Create=>s.peers'=s.peers+1]_s
MoveClosesNothing == [][Move=>s.roster'=s.roster /\ s.ended'=s.ended]_s
\* `peers` never exceeds the cap, because an open needs room: it is admitted
\* only while `Cap - Counted - peers > 0`, and it adds one to `peers` and
\* closes nothing. The Session's total can exceed the cap after a switch, when
\* a line that held the role and opened up to the cap starts counting again on
\* losing it, so `Counted + peers` becomes `peers + 1 > Cap`;
\* `CapOnlyRefusesNewOpens` is what answers that state, refusing the next open
\* and closing nothing.
PeersStayWithinCap == s.peers<=Cap
\* The boundary the review named, as its own specification: the root is the
\* default branch and one counted line is already open, so one slot is left.
\* `Next` admits the first open and refuses the second, and nothing closes a
\* line to make room.
BoundaryInit == RootInit /\ s.peers=Cap-1
BoundarySpec == BoundaryInit /\ [][Next]_s
\* From the boundary state, an open leaves no room, so the next one is refused.
\* This is what separates the old model, where a child of the default spent no
\* slot and two opens were admitted, from the corrected one.
OneSlotIsSpent == [][Create=>s.peers'=Cap]_s
\* The two switches the review named, each as its own specification.
\*
\* First: a cap-two Session with one open tool line, and the root is the default
\* branch, so that line is the Session's only counted line. Making it the default
\* branch stops it counting, so the counted total goes from one to zero, and
\* nothing is closed to get there.
MoveToChildInit == s = LineState("tool",FALSE,FALSE,0)
MoveToChildSpec == MoveToChildInit /\ [][Move]_s /\ WF_s(Move)
TheLineStaysOpen == ~s.ended
TheChildBecomesTheDefault == <>(s.primary)
\* The whole scenario in one statement: the Session starts with this one counted
\* line, and the switch takes the counted total to zero.
TheCountGoesFromOneToZero == (CountedTotal=1) ~> (CountedTotal=0)
\* The room BranchTool reads is the same number: the slot the line cost as an
\* open tool line off the path is free once the line holds the role.
MoveFreesTheSlot == [][Move=>RoomLeft'=Cap]_s
\* Second, the over-cap one: the Session is at its cap with two counted lines
\* elsewhere, and this line is an open tool line that is an ancestor of the
\* default branch, so the protected path runs through it and it spends no slot.
\* The role moving back to the root leaves it off that path, so it counts again:
\* the switch is admitted, the Session is over the cap, and no opening from it is
\* left. Nothing closes a line to make room, and the switch itself is decided by
\* the tree, not by the cap.
AtCapBelowInit == s = LineState("tool",FALSE,TRUE,Cap)
AtCapBelowSpec == AtCapBelowInit /\ [][DefaultLeavesBelow \/ Create]_s /\ WF_s(DefaultLeavesBelow)
TheSwitchIsAdmitted == <>(~s.descendantDefault)
TheSwitchLeavesTheSessionOverTheCap == [][DefaultLeavesBelow=>CountedTotal'>Cap]_s
\* The refusal is the same room every open reads, so no opener is left: the
\* root's own `create` and the default branch's would answer `branch_limit_reached`.
TheSwitchLeavesNoRoom == [][DefaultLeavesBelow=>RoomLeft'<0]_s
\* `Create` is a step of this specification so that the refusal is checked rather
\* than assumed: a model that let the switch leave room would be able to take it.
NoOpenInAnOverCapSession == ~s.descendantDefault=>RoomLeft<0
\* Admission in one order, first case: a line becomes the default branch only
\* while no ancestor's end is on its way down, so an end that already started
\* wins and the move is refused.
MoveNeedsOpenAncestry == [][s.primary' /\ ~s.primary=>~s.ancestorEnding]_s
\* Admission in one order, second case: a line starts ending only while no
\* descendant is the default branch, so a move that already happened wins and
\* the end is refused.
EndNeedsNoDefaultBelow == [][s.ending' /\ ~s.ending=>~s.descendantDefault]_s
\* A refused end writes nothing: `ending` moves only through an admitted end.
EndOnlyOnAllowedTarget == [][~CanBeEnded /\ ~CanEndSelf=>s.ending'=s.ending]_s
=============================================================================
