# Pull requests

Three pull requests are open against this repository.

1. `#41` — `retry-backoff`: adds exponential backoff to the upload retry loop. Touches `src/upload.py` and `tests/test_upload.py`; the tests are updated.
2. `#42` — `timezone-fix`: fixes the daily report boundary so it uses the local zone. Touches `src/report.py` only; no test changes.
3. `#43` — `lint-cleanup`: renames local variables and reformats two modules. Touches `src/report.py` and `src/upload.py`; no behaviour change.

The repository has no git remote; this file is the list of open pull requests.
