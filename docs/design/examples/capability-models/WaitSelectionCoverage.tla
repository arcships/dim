-------------------- MODULE WaitSelectionCoverage --------------------
EXTENDS WaitRuntime
VARIABLE order
vars == <<s, order>>
CoverageInit == Init /\ order = <<>>
CoverageNext ==
    /\ Next
    /\ order' = IF s.seen' # s.seen
                 THEN Append(order, CHOOSE i \in s.seen' \ s.seen: TRUE)
                 ELSE order
CoverageSpec == CoverageInit /\ [][CoverageNext]_vars
FirstPendingNotice ==
    [][(Len(s.selections') > Len(s.selections) /\ s.target = 0)
       => s.selections'[Len(s.selections')].task =
           Head(SelectSeq(order, LAMBDA i: i \in Queue))]_vars
=============================================================================
