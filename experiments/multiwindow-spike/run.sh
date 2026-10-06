#!/bin/sh
# M0.2 oracle runner: exit 0 only when the app exits 0, no crash line is logged and the
# mw-summary line reports every check true. usage: run.sh <exe> [frontend-dir]
# MW_LOG=<file> overrides the log path (default: mw-run.log in the working directory).
set -u
exe=${1:?usage: run.sh <exe> [frontend-dir]}
frontend=${2:-$(dirname "$0")}
log=${MW_LOG:-mw-run.log}
# On failure show why: the summary line and the log tail stay with the failing run.
fail() {
  echo "FAIL: $1"
  grep '"kind":"mw-summary"\|MW summary\|panicked\|content crashed' "$log" | tail -n 5
  echo "--- last log lines ($log):"
  tail -n 15 "$log"
  exit 1
}
ALEF_SPIKE_MULTIWINDOW=1 ALEF_TRANSPORT_SPIKE=1 ALEF_RESIZE_TRACE=1 \
  timeout 180 "$exe" --frontend-dir "$frontend" >"$log" 2>&1
code=$?
echo "exit=$code"
if [ "$code" -ne 0 ]; then
  fail "non-zero exit $code"
fi
if grep -qi "content crashed" "$log"; then
  fail "crash line in log"
fi
summary=$(grep '"kind":"mw-summary"' "$log" | tail -n 1)
if [ -z "$summary" ]; then
  fail "no mw-summary line"
fi
for key in w1_loaded w2_loaded resizes_confirmed timeouts_within_tolerance no_crash w2_closed_w1_alive clean_exit; do
  case "$summary" in
    *"\"$key\":true"*) ;;
    *) fail "check $key is not true" ;;
  esac
done
echo "mw oracle: PASS"
