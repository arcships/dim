-------------------- MODULE QueryRange --------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC
CONSTANT Kind, PageSize
VARIABLE s
Data == [kind : {"start", "end"}, id : {1, 2}]
Base(chain, cut) == (IF chain THEN 1..s.fork ELSE {}) \cup {i + 2 : i \in 1..cut}
Domain(chain, cut, from) == {p \in Base(chain, cut) : from = 0 \/ p >= from}
TailAt(bound, cut, from) == {p \in (3 + bound)..(2 + cut): from = 0 \/ p >= from}
Target == Domain(s.chain, s.cut, s.from)
Ids(q) == {s.history[p].id : p \in q}
Eligible(p, q) == IF Kind = "unmatched" THEN
    s.history[p].kind = "start" /\ ~\E r \in q:
        s.history[r].kind = "end" /\ s.history[r].id = s.history[p].id
    ELSE TRUE
Qualified == {p \in Target : Eligible(p, Target)}
Group(g) == {p \in Qualified : s.history[p].id = g}
Reference == CASE Kind = "unmatched" -> { [key |-> p, value |-> p] : p \in Qualified }
 [] Kind = "count" -> { [key |-> g, value |-> Cardinality(Group(g))] : g \in Ids(Qualified) }
 [] OTHER -> { [key |-> p, value |-> p, group |-> s.history[p].id] : p \in {p \in Qualified:
       \A r \in Group(s.history[p].id): IF Kind = "first" THEN p <= r ELSE p >= r }}
Before(a,b) == IF s.desc THEN a > b ELSE a < b
RECURSIVE Ordered(_)
Ordered(q) == IF q = {} THEN <<>> ELSE
    LET r == CHOOSE r \in q: \A t \in q: r = t \/ Before(r.key,t.key)
    IN <<r>> \o Ordered(q \ {r})
Prefix(n) == SubSeq(Ordered(Reference), 1, n)
Init == \E h \in [1..4 -> Data], fork \in {1,2}, desc \in BOOLEAN:
    s = [history |-> [p \in 1..5 |-> IF p <= 4 THEN h[p] ELSE [kind |-> "end",id |-> 1]],
         fork |-> fork, desc |-> desc, allocated |-> 2, published |-> 1,
         phase |-> "new", chain |-> FALSE, cut |-> 0, from |-> 0,
         bound |-> 0, tail |-> {}, disk |-> {}, input |-> {}, scan |-> 0,
         matches |-> {}, groups |-> [g \in 1..2 |-> 0],
         picks |-> [g \in 1..2 |-> 0], output |-> <<>>, after |-> 0,
         token |-> <<>>, error |-> "none"]
AppendSuffix == /\ s.allocated = 2 /\ s' = [s EXCEPT !.allocated = 3]
Publish == /\ s.published < s.allocated /\ s' = [s EXCEPT !.published = @ + 1]
Capture(chain, cut, from, internal) ==
    /\ s.phase = "new"
    /\ IF cut > (IF internal THEN s.allocated ELSE s.published)
           \/ ~(from = 0 \/ from \in Base(chain,cut))
        THEN s' = [s EXCEPT !.phase = "error", !.error = "invalidRange"]
        ELSE LET bound == IF s.published < cut THEN s.published ELSE cut
       IN s' = [s EXCEPT !.chain = chain, !.cut = cut, !.from = from,
                        !.bound = bound, !.tail = TailAt(bound,cut,from),
                        !.phase = "read"]
Read ==
    /\ s.phase = "read"
    /\ LET ancestors == IF s.chain THEN {p \in 1..s.fork:s.from = 0 \/ p >= s.from} ELSE {}
           current == {p \in 3..(2 + s.published):p <= 2 + s.bound /\ (s.from = 0 \/ p >= s.from)}
           disk == ancestors \cup current
       IN s' = [s EXCEPT !.disk = disk, !.input = disk \cup s.tail,
                        !.scan = 0, !.matches = {}, !.groups = [g \in 1..2 |-> 0],
                        !.picks = [g \in 1..2 |-> 0], !.phase = "scan"]
Scan ==
    /\ s.phase = "scan" /\ s.scan < 5
    /\ LET p == s.scan + 1 g == s.history[p].id
           keep == p \in s.input /\ Eligible(p,s.input)
       IN s' = [s EXCEPT !.scan = p,
                        !.matches = IF keep THEN @ \cup {p} ELSE @,
                        !.groups[g] = IF keep THEN @ + 1 ELSE @,
                        !.picks[g] = IF keep /\ (@ = 0 \/ Kind = "last") THEN p ELSE @]
Computed == CASE Kind = "unmatched" -> {[key |-> p,value |-> p]:p \in s.matches}
 [] Kind = "count" -> {[key |-> g,value |-> s.groups[g]]:g \in {g \in 1..2:s.groups[g]>0}}
 [] OTHER -> {[key |-> s.picks[g],value |-> s.picks[g],group |-> g]:g \in {g \in 1..2:s.picks[g]>0}}
Page ==
    /\ s.phase = "scan" /\ s.scan = 5
    /\ LET remaining == Ordered({r \in Computed:s.after = 0 \/ Before(s.after,r.key)})
           n == IF Len(remaining) < PageSize THEN Len(remaining) ELSE PageSize
           page == SubSeq(remaining,1,n)
       IN s' = [s EXCEPT !.output = @ \o page,
                        !.after = IF n > 0 THEN page[n].key ELSE @,
                        !.token = <<s.chain, s.cut, s.from, Kind>>,
                        !.phase = IF Len(remaining) > PageSize THEN "between" ELSE "done"]
NextPage(binding) ==
    /\ s.phase = "between"
    /\ IF binding # s.token
        THEN s' = [s EXCEPT !.phase = "error", !.error = "cursorMismatch"]
        ELSE LET bound == IF s.published < s.cut THEN s.published ELSE s.cut
       IN s' = [s EXCEPT !.bound = bound,
                        !.tail = TailAt(bound,s.cut,s.from), !.phase = "read"]
StorageError ==
    /\ s.phase \in {"read","between"}
    /\ s' = [s EXCEPT !.phase = "error", !.error = "storage"]
Next == AppendSuffix \/ Publish \/ Read \/ Scan \/ Page
    \/ (\E binding \in {<<s.chain,s.cut,s.from,Kind>>,
          <<s.chain,s.cut+1,s.from,Kind>>, <<s.chain,s.cut,s.from,"differentQuery">>}:NextPage(binding))
    \/ (\E chain \in BOOLEAN, cut \in 0..4, from \in 0..6, internal \in BOOLEAN:
          Capture(chain,cut,from,internal))
    \/ StorageError
Spec == Init /\ [][Next]_s
SourcePartition == s.phase \in {"scan","between","done"} =>
    /\ s.disk \cap s.tail = {} /\ s.input = Target
    /\ s.disk \cup s.tail = Target
ScanAgrees == s.phase = "scan" /\ s.scan = 5 => Computed = Reference
PagesArePrefix == s.output = Prefix(Len(s.output))
CompleteResult == s.phase = "done" => s.output = Ordered(Reference)
ErrorIsNotEmptySuccess == s.phase = "error" => s.error # "none"
FixedRange == [][s.phase # "new" =>
    s.cut' = s.cut /\ s.chain' = s.chain /\ s.from' = s.from]_s
=============================================================================
