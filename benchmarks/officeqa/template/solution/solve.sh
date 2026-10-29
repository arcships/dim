#!/bin/bash
set -euo pipefail
cat > /app/answer.txt <<'OFFICEQA_ANSWER_END'
{{answer}}
OFFICEQA_ANSWER_END
