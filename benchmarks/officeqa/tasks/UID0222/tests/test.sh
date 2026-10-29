#!/bin/bash
# No fallback reward. A verifier that crashed has not graded anything, and a 0
# written on its behalf would score a broken verifier as a failed agent.
set -euo pipefail
mkdir -p /logs/verifier
exec python3 /tests/evaluate.py
