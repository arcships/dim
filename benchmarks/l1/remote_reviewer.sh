#!/bin/bash
# Run the L1 judge on the eval machine instead of locally.
#
# The judge needs a working route to the model endpoint, and a developer machine
# often does not have one — a proxy in fake-ip mode resolves the endpoint to a
# reserved address and every call fails DNS. The eval box already holds the
# credential and talks to the endpoint continuously, so it is the natural place
# to run this. CI would be the other.
#
# stdin (prompt + diff) is piped through ssh; nothing is written to disk.
set -euo pipefail
HOST="${DIM_EVAL_JUDGE_HOST:?DIM_EVAL_JUDGE_HOST names the eval machine}"
REMOTE="${DIM_EVAL_JUDGE_SCRIPT:?DIM_EVAL_JUDGE_SCRIPT names the delivered judge}"
exec ssh -o ConnectTimeout=15 "$HOST" \
  "set -a; . ~/.dim-eval-env; set +a; python3 $REMOTE"
