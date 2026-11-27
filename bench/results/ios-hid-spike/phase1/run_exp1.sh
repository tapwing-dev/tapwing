#!/usr/bin/env bash
# Phase 1, experiment 1: location alert with no runner alive, dismissed through the HID probe.
# Usage: run_exp1.sh <cycles>   Output: exp1/cycle-N.* (raw), exp1/results.txt
set -u
repo=$(cd "$(dirname "$0")/../../../.." && pwd)
cd "$repo"
U=32E8A0E5-D41A-4CC4-BCD4-39E9809FAA7D
P=bench/ios/hid-probe/build/hid-probe
O=bench/results/ios-hid-spike/phase1/exp1/${RUN:-runB}
: > $O/results.txt
for i in $(seq 1 "$1"); do
  ./crates/tapwing/target/release/tapwing test bench/results/ios-hid-spike/phase1/flows/alert_up.yaml \
    --device $U --input flutter > $O/cycle-$i.tapwing.log 2>&1; rc=$?
  runners=$(pgrep -fl "xcodebuild|ios-runnerUITests" | grep -c $U)
  $P snapshot $U --out $O/cycle-$i.before.json 2> $O/cycle-$i.before.err
  $P frame $U $O/cycle-$i.before.png 2>/dev/null
  alert=$(python3 -I - $O/cycle-$i.before.json <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
labels=[]
def w(n):
    if n.get('label'): labels.append((n['role'],n['label'],n['frame']))
    for c in n['children']: w(c)
for r in d['roots']: w(r)
btn=[l for l in labels if l[0]=='AXButton']
txt=[l for l in labels if l[0]=='AXStaticText']
want=[b for b in btn if b[1]=='Povolit jen při používání aplikace']
print(len(btn),len(txt),(want[0][2][0]+want[0][2][2]/2, want[0][2][1]+want[0][2][3]/2) if want else 'none')
PY
)
  set -- $alert; nb=$1; nt=$2; cx=${3//[(,]/}; cy=${4//[)]/}
  granted_ms=none
  if [ "$cx" != none ]; then
    $P tap $U $cx $cy 2> $O/cycle-$i.tap.err
    t0=$(perl -MTime::HiRes=time -e 'print time')
    # Poll for up to 6 s: when does the app show "Permission: granted"?
    for k in $(seq 1 40); do
      $P snapshot $U --out $O/cycle-$i.after.json 2> $O/cycle-$i.after.err
      if grep -q "Permission: granted" $O/cycle-$i.after.json; then
        granted_ms=$(perl -MTime::HiRes=time -e "printf '%d', (time-$t0)*1000"); break; fi
      sleep 0.15
    done
  fi
  granted=$(grep -c "Permission: granted" $O/cycle-$i.after.json)
  alertgone=$(grep -c "Povolit jen" $O/cycle-$i.after.json)
  echo "cycle $i tapwing_rc=$rc runners_alive=$runners buttons=$nb texts=$nt tap=($cx,$cy) granted_in_app=$granted granted_after_ms=$granted_ms alert_still_there=$alertgone" | tee -a $O/results.txt
done
