"""L2: build a job, run it, score it, compare it against the baseline.

This file exists so `unittest discover` can reach `runner/flow_test.py`. Discovery
walks into a directory only if it is an importable package, and without this one
it walked past — silently, reporting OK over the 63 tests it did find while the 24
in here never ran. Do not delete it because it looks empty.

Nothing is re-exported. The modules here are entry points as much as they are
imports (`run_l2.py`, `baseline.py freeze`, `calibrate.py`), and they add their own
directory to `sys.path` so that works either way.
"""
