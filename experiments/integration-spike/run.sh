#!/bin/sh
# M0.3 integration spike runner.
# usage: sh run.sh full|nodialog|baseline [break-forwarding]
# Exits non-zero unless the app exits 0 and every required verdict check is true.
set -u
mode=${1:?usage: run.sh full|nodialog|baseline [break-forwarding]}
break_flag=${2:-}
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
# EXE overrides the binary; default is the debug build of this checkout.
if [ -z "${EXE:-}" ]; then
  exe="$root/backend/target/debug/alef-file-manager"
  [ -x "$exe" ] || exe="$exe.exe"
else
  exe="$EXE"
fi
scratch=$root/backend/target/m0-integration-scratch
log=""
verdict=""
mkdir -p "$scratch"
trap 'rm -f "$log" "$verdict"; rmdir "$scratch" 2>/dev/null' EXIT
[ -x "$exe" ] || { echo "FAIL: missing exe: $exe" >&2; exit 2; }
suffix=""
[ -n "$break_flag" ] && suffix="-$break_flag"
verdict="$scratch/verdict-$mode$suffix.json"
log="$scratch/run-$mode$suffix.log"
rm -f "$verdict"
case "$mode" in
  full) base_env="ALEF_SPIKE_INTEGRATION=1" ;;
  nodialog) base_env="ALEF_SPIKE_INTEGRATION=1 ALEF_SPIKE_INTEGRATION_NO_DIALOG=1" ;;
  baseline) base_env="ALEF_SPIKE_RESIZE_ONLY=1" ;;
  *) echo "FAIL: unknown mode $mode" >&2; exit 2 ;;
esac
[ -n "$break_flag" ] && base_env="$base_env ALEF_SPIKE_BREAK=${break_flag#break-}"
eval "env \
  ALEF_TRANSPORT_SPIKE=1 \
  ALEF_RESIZE_TRACE=1 \
  ALEF_SPIKE_VERDICT_FILE=\"$verdict\" \
  $base_env \
  timeout 90 \"$exe\" --frontend-dir \"$here\"" >"$log" 2>&1
code=$?
echo "app exit: $code"
if [ "$code" -ne 0 ]; then echo "FAIL: app exit $code (log: $log)"; exit 1; fi
if [ ! -f "$verdict" ]; then echo "FAIL: verdict file missing"; exit 1; fi
echo "verdict: $(cat "$verdict")"
case "$mode" in
  baseline) required="resize_no_timeouts shutdown_clean" ;;
  *) required="created forwarding dialog_render_continuity resize_no_timeouts shutdown_clean" ;;
esac
rc=0
for check in $required; do
  if grep -q "\"$check\":false" "$verdict"; then
    echo "FAIL: $check=false"; rc=1
  elif grep -q "\"$check\":true" "$verdict"; then
    echo "PASS: $check=true"
  else
    echo "FAIL: $check not in verdict"; rc=1
  fi
done
grep -q '"verdict":"ok"' "$verdict" || { echo "FAIL: verdict is not ok"; rc=1; }
if [ "$rc" -eq 0 ]; then echo "RUN PASS ($mode)"; else echo "RUN FAIL ($mode)"; fi
exit $rc
