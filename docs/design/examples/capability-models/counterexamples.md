# ShellSession recovery counterexamples

These are the actual temporal traces from the recorded TLC run, not manually constructed test failures. See [the run record](results.md) for hashes and [the model](ExecLifecycle.tla) for actions. `started`, `ended`, `eligible` and `wake` represent durable facts/eligibility; `live` and `holds` are generation-local handles. `Restore` does not restore those handles.

The final stuttering state witnesses that the requested recovery obligation can remain unfulfilled. Normal close is a different path. The trace is not necessarily the shortest counterexample. No real host crash was performed.

## ExecLifecycle-Shell: RecoveredSettled

Configuration: [ExecLifecycle-Shell.cfg](ExecLifecycle-Shell.cfg). Exit status: 13.

```text
State 1: <Initial predicate>
s = [ started |-> {},
  live |-> {},
  released |-> {},
  ended |-> {},
  wake |-> {},
  eligible |-> {},
  holds |-> {},
  crashed |-> FALSE,
  restored |-> FALSE,
  closing |-> FALSE,
  inherited |-> {},
  stopped |-> FALSE ]

State 2: <StopTurn line 19, col 13 to line 19, col 40 of module ExecLifecycle>
s = [ started |-> {},
  live |-> {},
  released |-> {},
  ended |-> {},
  wake |-> {},
  eligible |-> {},
  holds |-> {},
  crashed |-> FALSE,
  restored |-> FALSE,
  closing |-> FALSE,
  inherited |-> {},
  stopped |-> TRUE ]

State 3: <Start line 9, col 13 to line 10, col 71 of module ExecLifecycle>
s = [ started |-> {1},
  live |-> {1},
  released |-> {},
  ended |-> {},
  wake |-> {},
  eligible |-> {},
  holds |-> {1},
  crashed |-> FALSE,
  restored |-> FALSE,
  closing |-> FALSE,
  inherited |-> {},
  stopped |-> TRUE ]

State 4: <Release line 11, col 15 to line 12, col 51 of module ExecLifecycle>
s = [ started |-> {1},
  live |-> {},
  released |-> {1},
  ended |-> {},
  wake |-> {},
  eligible |-> {},
  holds |-> {1},
  crashed |-> FALSE,
  restored |-> FALSE,
  closing |-> FALSE,
  inherited |-> {},
  stopped |-> TRUE ]

State 5: <Crash line 21, col 10 to line 22, col 55 of module ExecLifecycle>
s = [ started |-> {1},
  live |-> {},
  released |-> {1},
  ended |-> {},
  wake |-> {},
  eligible |-> {},
  holds |-> {},
  crashed |-> TRUE,
  restored |-> FALSE,
  closing |-> FALSE,
  inherited |-> {},
  stopped |-> TRUE ]

State 6: <Restore line 23, col 12 to line 24, col 50 of module ExecLifecycle>
s = [ started |-> {1},
  live |-> {},
  released |-> {1},
  ended |-> {},
  wake |-> {},
  eligible |-> {},
  holds |-> {},
  crashed |-> FALSE,
  restored |-> TRUE,
  closing |-> FALSE,
  inherited |-> {},
  stopped |-> TRUE ]

State 7: <Close line 20, col 10 to line 20, col 54 of module ExecLifecycle>
s = [ started |-> {1},
  live |-> {},
  released |-> {1},
  ended |-> {},
  wake |-> {},
  eligible |-> {},
  holds |-> {},
  crashed |-> FALSE,
  restored |-> TRUE,
  closing |-> TRUE,
  inherited |-> {},
  stopped |-> TRUE ]

State 8: Stuttering
```

## ExecLifecycle-ShellWake: RecoveredWoken

Configuration: [ExecLifecycle-ShellWake.cfg](ExecLifecycle-ShellWake.cfg). Exit status: 13.

```text
State 1: <Initial predicate>
s = [ started |-> {},
  live |-> {},
  released |-> {},
  ended |-> {},
  wake |-> {},
  eligible |-> {},
  holds |-> {},
  crashed |-> FALSE,
  restored |-> FALSE,
  closing |-> FALSE,
  inherited |-> {},
  stopped |-> FALSE ]

State 2: <Start line 9, col 13 to line 10, col 71 of module ExecLifecycle>
s = [ started |-> {2},
  live |-> {2},
  released |-> {},
  ended |-> {},
  wake |-> {},
  eligible |-> {},
  holds |-> {2},
  crashed |-> FALSE,
  restored |-> FALSE,
  closing |-> FALSE,
  inherited |-> {},
  stopped |-> FALSE ]

State 3: <Release line 11, col 15 to line 12, col 51 of module ExecLifecycle>
s = [ started |-> {2},
  live |-> {},
  released |-> {2},
  ended |-> {},
  wake |-> {},
  eligible |-> {},
  holds |-> {2},
  crashed |-> FALSE,
  restored |-> FALSE,
  closing |-> FALSE,
  inherited |-> {},
  stopped |-> FALSE ]

State 4: <End line 13, col 11 to line 15, col 46 of module ExecLifecycle>
s = [ started |-> {2},
  live |-> {},
  released |-> {2},
  ended |-> {2},
  wake |-> {},
  eligible |-> {2},
  holds |-> {},
  crashed |-> FALSE,
  restored |-> FALSE,
  closing |-> FALSE,
  inherited |-> {},
  stopped |-> FALSE ]

State 5: <Start line 9, col 13 to line 10, col 71 of module ExecLifecycle>
s = [ started |-> {1, 2},
  live |-> {1},
  released |-> {2},
  ended |-> {2},
  wake |-> {},
  eligible |-> {2},
  holds |-> {1},
  crashed |-> FALSE,
  restored |-> FALSE,
  closing |-> FALSE,
  inherited |-> {},
  stopped |-> FALSE ]

State 6: <Release line 11, col 15 to line 12, col 51 of module ExecLifecycle>
s = [ started |-> {1, 2},
  live |-> {},
  released |-> {1, 2},
  ended |-> {2},
  wake |-> {},
  eligible |-> {2},
  holds |-> {1},
  crashed |-> FALSE,
  restored |-> FALSE,
  closing |-> FALSE,
  inherited |-> {},
  stopped |-> FALSE ]

State 7: <Close line 20, col 10 to line 20, col 54 of module ExecLifecycle>
s = [ started |-> {1, 2},
  live |-> {},
  released |-> {1, 2},
  ended |-> {2},
  wake |-> {},
  eligible |-> {2},
  holds |-> {1},
  crashed |-> FALSE,
  restored |-> FALSE,
  closing |-> TRUE,
  inherited |-> {},
  stopped |-> FALSE ]

State 8: <End line 13, col 11 to line 15, col 46 of module ExecLifecycle>
s = [ started |-> {1, 2},
  live |-> {},
  released |-> {1, 2},
  ended |-> {1, 2},
  wake |-> {},
  eligible |-> {2},
  holds |-> {},
  crashed |-> FALSE,
  restored |-> FALSE,
  closing |-> TRUE,
  inherited |-> {},
  stopped |-> FALSE ]

State 9: <Crash line 21, col 10 to line 22, col 55 of module ExecLifecycle>
s = [ started |-> {1, 2},
  live |-> {},
  released |-> {1, 2},
  ended |-> {1, 2},
  wake |-> {},
  eligible |-> {2},
  holds |-> {},
  crashed |-> TRUE,
  restored |-> FALSE,
  closing |-> TRUE,
  inherited |-> {},
  stopped |-> FALSE ]

State 10: <Restore line 23, col 12 to line 24, col 50 of module ExecLifecycle>
s = [ started |-> {1, 2},
  live |-> {},
  released |-> {1, 2},
  ended |-> {1, 2},
  wake |-> {},
  eligible |-> {2},
  holds |-> {},
  crashed |-> FALSE,
  restored |-> TRUE,
  closing |-> TRUE,
  inherited |-> {},
  stopped |-> FALSE ]

State 11: <StopTurn line 19, col 13 to line 19, col 40 of module ExecLifecycle>
s = [ started |-> {1, 2},
  live |-> {},
  released |-> {1, 2},
  ended |-> {1, 2},
  wake |-> {},
  eligible |-> {2},
  holds |-> {},
  crashed |-> FALSE,
  restored |-> TRUE,
  closing |-> TRUE,
  inherited |-> {},
  stopped |-> TRUE ]

State 12: Stuttering
```
