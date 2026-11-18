# Wait selection coverage counterexample

Historical run at `5a78b2ed2`: the expanded WaitRuntime passes the observation probe as restated for #1821's pending set; see [the follow-up](coverage-fill.md). The trace and original hashes below are preserved.

This is a model-coverage counterexample, not an implementation defect. The probe observes the existing WaitRuntime transitions without restricting them. See [the coverage analysis](coverage-audit.md).

TLC exit: 13. Search stopped at the counterexample: 676 generated, 427 distinct, depth 6, 275 states still queued. This is not an exhaustive passing run.

Artifact SHA-256:

- `WaitRuntime.tla`: `eb0b1922df1d4e7b34fead5bc0976dfab5b7b922afbe491cebe3c8799790b581`
- `WaitSelectionCoverage.tla`: `26dbb9b272fc93bb37a1f2ab1f75ee5477b8fb12eac4fa82751799c2510aa12c`
- `WaitSelectionCoverage.cfg`: `1938ab1ccbfa29d9fd57a955b9b5ca8c114d90b5d064ede6404c3c0b9f41ca1f`

```text
State 1: <Initial predicate>
/\ order = <<>>
/\ s = [ seen |-> {},
  selections |-> <<>>,
  target |-> 0,
  arrivals |-> {},
  open |-> FALSE,
  taken |-> {},
  waiting |-> 0,
  closing |-> FALSE,
  tracked |-> {},
  finished |-> {},
  generation |-> 0,
  accepted |-> 0,
  boundaries |-> <<>>,
  recovered |-> FALSE ]

State 2: <CoverageNext line 7, col 5 to line 10, col 27 of module WaitSelectionCoverage>
/\ order = <<>>
/\ s = [ seen |-> {},
  selections |-> <<>>,
  target |-> 0,
  arrivals |-> {},
  open |-> TRUE,
  taken |-> {},
  waiting |-> 0,
  closing |-> FALSE,
  tracked |-> {},
  finished |-> {},
  generation |-> 0,
  accepted |-> 0,
  boundaries |-> <<TRUE>>,
  recovered |-> FALSE ]

State 3: <CoverageNext line 7, col 5 to line 10, col 27 of module WaitSelectionCoverage>
/\ order = <<1>>
/\ s = [ seen |-> {1},
  selections |-> <<>>,
  target |-> 0,
  arrivals |-> {1},
  open |-> TRUE,
  taken |-> {},
  waiting |-> 0,
  closing |-> FALSE,
  tracked |-> {},
  finished |-> {},
  generation |-> 0,
  accepted |-> 0,
  boundaries |-> <<TRUE>>,
  recovered |-> FALSE ]

State 4: <CoverageNext line 7, col 5 to line 10, col 27 of module WaitSelectionCoverage>
/\ order = <<1, 2>>
/\ s = [ seen |-> {1, 2},
  selections |-> <<>>,
  target |-> 0,
  arrivals |-> {1, 2},
  open |-> TRUE,
  taken |-> {},
  waiting |-> 0,
  closing |-> FALSE,
  tracked |-> {},
  finished |-> {},
  generation |-> 0,
  accepted |-> 0,
  boundaries |-> <<TRUE>>,
  recovered |-> FALSE ]

State 5: <CoverageNext line 7, col 5 to line 10, col 27 of module WaitSelectionCoverage>
/\ order = <<1, 2>>
/\ s = [ seen |-> {1, 2},
  selections |-> <<>>,
  target |-> 0,
  arrivals |-> {1, 2},
  open |-> TRUE,
  taken |-> {},
  waiting |-> 1,
  closing |-> FALSE,
  tracked |-> {1},
  finished |-> {},
  generation |-> 0,
  accepted |-> 1,
  boundaries |-> <<TRUE>>,
  recovered |-> FALSE ]

State 6: <CoverageNext line 7, col 5 to line 10, col 27 of module WaitSelectionCoverage>
/\ order = <<1, 2>>
/\ s = [ seen |-> {1, 2},
  selections |-> <<[target |-> 0, task |-> 2]>>,
  target |-> 0,
  arrivals |-> {1},
  open |-> TRUE,
  taken |-> {2},
  waiting |-> 0,
  closing |-> FALSE,
  tracked |-> {1},
  finished |-> {1},
  generation |-> 0,
  accepted |-> 1,
  boundaries |-> <<TRUE>>,
  recovered |-> FALSE ]

676 states generated, 427 distinct states found, 275 states left on queue.
The depth of the complete state graph search is 6.
The average outdegree of the complete state graph is 3 (minimum is 0, the maximum 8 and the 95th percentile is 6).
Finished in 00s at (2026-11-15 23:20:48)
```
