#!/usr/bin/env bash
# The scheduled eval, run on the eval machine by cron once per suite:
#
#   nightly.sh [frontier|officeqa-pro] [daily|checkpoint] [daily_run.py options...]
#
# Extra options go to daily_run.py, e.g. `--dry-run` to judge the window and
# build nothing when trying out a new setup; issues are then printed, not filed.
# It evaluates whatever origin/main points at, writes the judgement records to
# the ledger branch, has each failed trial analyzed, and files the night's
# failures and any pipeline problem as GitHub issues. Settings live
# in $DIM_EVAL_HOME/config.env; benchmarks/host/README.md lists them and the
# one-time setup. Exit codes are daily_run.py's, plus 8 when the ledger push
# failed and 10 when issues could not be filed (benchmarks/README.md).
#
# The body is one function called on the last line: the script checks out a
# new main under itself, and bash reads a file as it runs it, so the whole
# script has to be parsed before the checkout moves it.
set -euo pipefail

main() {
  local suite="${1:-frontier}" run_type="${2:-daily}"
  shift $(( $# < 2 ? $# : 2 ))
  local home="${DIM_EVAL_HOME:-$HOME/dim-eval}"
  set -a
  # shellcheck source=/dev/null
  . "$home/config.env"
  set +a
  local remote="${DIM_EVAL_GIT_REMOTE:-git@github.com:arcships/dim.git}"
  local ledger_remote="${DIM_EVAL_LEDGER_REMOTE:-$remote}"
  local ledger_branch="${DIM_EVAL_LEDGER_BRANCH:-eval-ledger}"
  local github_repo="${DIM_EVAL_GITHUB_REPO:-arcships/dim}"

  # A baseline is the reference every later comparison reads; replacing it is
  # a reviewed change made by hand, never a side effect of the schedule.
  case "$run_type" in
    daily|checkpoint) ;;
    *) echo "run type $run_type is not scheduled; take a baseline by hand" >&2; exit 1 ;;
  esac
  # Each suite advances its own window, so each has its own state file.
  local tasks state concurrency
  case "$suite" in
    frontier)
      tasks=benchmarks/tasks state=daily-state.json concurrency=5 ;;
    officeqa-pro)
      # k=3 is 399 trials, about 20 hours at worst: run that by hand.
      [ "$run_type" = daily ] || { echo "officeqa-pro runs daily only" >&2; exit 1; }
      tasks=benchmarks/officeqa/tasks state=officeqa-pro-state.json concurrency=10 ;;
    *) echo "unknown suite $suite (frontier or officeqa-pro)" >&2; exit 1 ;;
  esac

  mkdir -p "$home/logs" "$home/results"
  # One run at a time: two runs would share the remote results directory and
  # judge the same window twice. A later run waits for the earlier one.
  exec 9>"$home/nightly.lock"
  flock -w 72000 9 || { echo "[nightly] another run held the lock for 20 hours" >&2; exit 1; }
  local log
  log="$home/logs/$(date -u +%Y%m%dT%H%M%SZ)-$suite-$run_type.log"
  exec > >(tee -a "$log") 2>&1
  echo "[nightly] $suite $run_type, log $log"

  local repo="$home/repo" ledger="$home/ledger" started="$home/.started-$suite"
  touch "$started"
  local status=0 pushed=1 dry=()
  case " $* " in *" --dry-run "*) dry=(--dry-run) ;; esac
  prepare "$repo" "$remote" "$ledger" "$ledger_remote" "$ledger_branch" "$state" || status=$?

  if [ "$status" -eq 0 ]; then
    local head
    head="$(git -C "$repo" rev-parse HEAD)"
    # The harness and the subject come from the same commit of main: the
    # harness must be main's, and main is what is evaluated.
    python3 "$repo/benchmarks/daily_run.py" \
      --repo "$repo" \
      --run-type "$run_type" \
      --head "$head" \
      --no-fetch \
      --l2-timeout-hours 9 \
      --remote-judge \
      --ledger-dir "$ledger" \
      --tasks-dir "$repo/$tasks" \
      --concurrency "$concurrency" \
      --state "$ledger/$state" \
      --cache-dir "$home/l1-cache" \
      --out "$home/results" \
      --project-id "$github_repo" "$@" || status=$?
    push_ledger "$ledger" "$ledger_branch" "$state" "$suite" "$run_type" "$head" || {
      echo "[nightly] WARNING: ledger push failed; the commit stays in $ledger and"
      echo "[nightly] is pushed with the next run's records"
      pushed=0
      [ "$status" -eq 0 ] && status=8
    }
  fi

  local run_json analysis="" analysis_failed=0
  run_json="$(find "$home/results" -mindepth 2 -maxdepth 2 -name run.json -newer "$started" | sort | tail -n 1)"
  local known="$home/known-failures-$suite.json"
  local known_next="$home/known-failures-$suite.next.json"
  # A leftover from an interrupted run must never be committed by a later run
  # that did not perform analysis itself.
  rm -f "$known_next"
  # Every failed trial is explained by a model before it is reported. The
  # model's key comes from the same file the L1 judge reads, in a subshell, so
  # it is in no other step's environment. The updated known-failure record is
  # written beside the current one and only committed after issues.py succeeds:
  # an alerting failure must not teach the next night that these failures were
  # already reported.
  if [ -n "$run_json" ] && [ -d "$(dirname "$run_json")/harbor" ] && [ "${#dry[@]}" -eq 0 ]; then
    analysis="$(dirname "$run_json")/analysis.json"
    ( set -a; . "$HOME/.dim-eval-env"; set +a
      python3 "$repo/benchmarks/report/analyze.py" --run-dir "$(dirname "$run_json")" \
        --repo "$repo" --known "$known" --known-next "$known_next" --out "$analysis" ) \
      || { echo "[nightly] WARNING: failure analysis did not finish"; analysis=""; analysis_failed=1; }
  fi
  local push_flag=()
  [ "$pushed" -eq 1 ] || push_flag=(--ledger-push-failed)
  local analysis_flag=()
  [ "$analysis_failed" -eq 1 ] && analysis_flag=(--analysis-failed)
  if python3 "$repo/benchmarks/report/issues.py" \
    --repo "$github_repo" --suite "$suite" --run-type "$run_type" \
    --exit-code "$status" ${run_json:+--run-json "$run_json"} ${analysis:+--analysis "$analysis"} \
    "${push_flag[@]}" "${analysis_flag[@]}" "${dry[@]}" \
    --log "$log" --token-file "$home/github-token"; then
    if [ -f "$known_next" ]; then
      mv -f "$known_next" "$known" || {
        echo "[nightly] WARNING: could not commit $known"
        status=10
      }
    fi
  else
    echo "[nightly] WARNING: issues could not be filed; alerting is down (eval exit $status)"
    rm -f "$known_next"
    status=10
  fi

  # Harbor output and the eval machine's staging copies are kept for 30 days.
  local dir
  for dir in "$home/results" "${DIM_EVAL_SCRATCH:-}"; do
    [ -n "$dir" ] && [ -d "$dir" ] || continue
    find "$dir" -mindepth 1 -maxdepth 1 -type d -mtime +30 -exec chmod -R u+w {} + -exec rm -rf {} + \
      || echo "[nightly] WARNING: could not prune $dir"
  done
  echo "[nightly] exit $status"
  exit "$status"
}

# Called as `prepare ... || status=$?`, where bash ignores `set -e`; every
# step therefore returns on failure itself.
prepare() {
  local repo="$1" remote="$2" ledger="$3" ledger_remote="$4" branch="$5" state="$6"
  if [ ! -d "$repo/.git" ]; then git clone --quiet "$remote" "$repo" || return 1; fi
  git -C "$repo" fetch --quiet origin main || return 1
  git -C "$repo" checkout --quiet --force --detach origin/main || return 1
  git -C "$repo" clean -fdq || return 1

  # The ledger clone persists across runs, so a commit whose push failed is
  # rebased and pushed with the next run's records.
  if [ -d "$ledger/.git" ]; then
    # A rebase that conflicted, or records a killed run left uncommitted,
    # would otherwise make every later pull refuse.
    git -C "$ledger" rebase --abort >/dev/null 2>&1 || true
    local path
    for path in merges runs "$state"; do
      if [ -e "$ledger/$path" ]; then git -C "$ledger" add -A -- "$path" || return 1; fi
    done
    if ! git -C "$ledger" diff --cached --quiet; then
      git -C "$ledger" commit --quiet -m "eval: records left by an interrupted run" || return 1
    fi
    git -C "$ledger" pull --quiet --rebase origin "$branch" || {
      git -C "$ledger" rebase --abort >/dev/null 2>&1; return 1; }
  else
    git clone --quiet --branch "$branch" --single-branch "$ledger_remote" "$ledger" || return 1
    git -C "$ledger" config user.name "dim eval"
    git -C "$ledger" config user.email "dim-eval@users.noreply.github.com"
  fi
  # The window start is never guessed (benchmarks/README.md, 窗口).
  if [ ! -f "$ledger/$state" ]; then
    echo "[nightly] $ledger/$state is missing: add {\"last_l2_head\": \"<sha>\"} to $branch" >&2
    return 1
  fi
}

push_ledger() {
  local ledger="$1" branch="$2" state="$3" suite="$4" run_type="$5" head="$6"
  # Only the ledger's own files: what lands on the branch is decided here, not
  # by whatever else appeared in the clone.
  local path
  for path in merges runs "$state"; do
    if [ -e "$ledger/$path" ]; then git -C "$ledger" add -A -- "$path" || return 1; fi
  done
  if ! git -C "$ledger" diff --cached --quiet; then
    git -C "$ledger" commit --quiet -m "eval: $suite $run_type ${head:0:9}" || return 1
  fi
  [ -z "$(git -C "$ledger" log --oneline "origin/$branch..HEAD")" ] && return 0
  git -C "$ledger" push --quiet origin "HEAD:$branch" && return 0
  if git -C "$ledger" pull --quiet --rebase origin "$branch"; then
    git -C "$ledger" push --quiet origin "HEAD:$branch"
  else
    git -C "$ledger" rebase --abort >/dev/null 2>&1
    return 1
  fi
}

main "$@"
