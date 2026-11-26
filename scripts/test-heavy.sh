#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export HEADWORK_HEAVY_DIR="$work/slots" HEADWORK_HEAVY_SLOTS=2 HEADWORK_HEAVY_REPORT=1
unset HEADWORK_HEAVY_HELD CI HEADWORK_HEAVY_OFF
failures=0

check() {
    if eval "$2"; then
        echo "ok: $1"
    else
        echo "FAIL: $1"
        tail -n 20 "$work"/*.err 2>/dev/null || true
        failures=$((failures + 1))
    fi
}

job() {
    scripts/heavy.sh bash -c "echo start \$(date +%s) >> '$work/$1'; sleep 2; echo end \$(date +%s) >> '$work/$1'" \
        2> "$work/$1.err"
}

job a &
job b &
sleep 0.5
job c &
wait
check "the third job waited with a message" "grep -q 'waiting' '$work/c.err'"
check "the first two did not wait" "! grep -q 'waiting' '$work/a.err' && ! grep -q 'waiting' '$work/b.err'"
first_end=$(sed -n 's/^end //p' "$work/a" "$work/b" | sort -n | head -n 1)
third_start=$(sed -n 's/^start //p' "$work/c")
check "the third started once a slot was freed" "[ '$third_start' -ge '$first_end' ]"
check "every slot is released" "[ -z \"\$(ls -A '$work/slots' 2>/dev/null)\" ]"

HEADWORK_HEAVY_SLOTS=1 scripts/heavy.sh bash -c "HEADWORK_HEAVY_NO_WAIT=1 scripts/heavy.sh scripts/heavy.sh true" \
    2> "$work/nested.err" && nested=0 || nested=$?
check "a nested job runs in its parent's slot" "[ '$nested' -eq 0 ] && ! grep -q 'no free slot' '$work/nested.err'"

check "the default is three slots" "[ \"\$(env -u HEADWORK_HEAVY_SLOTS bash -c '. scripts/heavy.sh; heavy_slots')\" = 3 ]"
check "a bad slot count falls back to three" "[ \"\$(HEADWORK_HEAVY_SLOTS=x bash -c '. scripts/heavy.sh; heavy_slots')\" = 3 ]"

mkdir -p "$work/slots/slot-1"
bash -c 'exit 0' &
gone=$!
wait "$gone"
printf 'pid=%s\nworktree=gone\ncommand=dead job\nstarted=then\n' "$gone" > "$work/slots/slot-1/owner"
check "status calls the dead holder stale" "scripts/heavy.sh status | grep -q 'slot-1 (stale)'"
HEADWORK_HEAVY_SLOTS=1 scripts/heavy.sh --no-wait true 2> "$work/stale.err" && stale=0 || stale=$?
check "a stale slot is taken over" "[ '$stale' -eq 0 ] && grep -q 'taking over slot-1' '$work/stale.err'"

mkdir -p "$work/slots/slot-1"
printf 'pid=%s\nworktree=here\ncommand=live job\nstarted=now\n' "$$" > "$work/slots/slot-1/owner"
HEADWORK_HEAVY_SLOTS=1 scripts/heavy.sh --no-wait true 2> "$work/busy.err" && busy=0 || busy=$?
check "--no-wait fails at once when every slot is held" "[ '$busy' -eq 75 ] && grep -q 'live job' '$work/busy.err'"
CI=true HEADWORK_HEAVY_SLOTS=1 scripts/heavy.sh true && ci=0 || ci=$?
check "in CI a full slot folder does not block" "[ '$ci' -eq 0 ]"
rm -rf "$work/slots/slot-1"

touch "$work/blocker"
HEADWORK_HEAVY_DIR="$work/blocker/slots" scripts/heavy.sh true 2> "$work/nodir.err" && nodir=0 || nodir=$?
check "an unusable slot folder falls back to running directly" "[ '$nodir' -eq 0 ] && grep -q 'without a slot' '$work/nodir.err'"

mkdir -p "$work/slots/slot-1"
: > "$work/slots/slot-1/owner"
HEADWORK_HEAVY_SLOTS=1 scripts/heavy.sh --no-wait true 2> "$work/young.err" && young=0 || young=$?
check "a young slot with an unwritten owner is not taken over" "[ '$young' -eq 75 ]"
touch -t "$(date -v-30S +%Y%m%d%H%M.%S 2>/dev/null || date -d '30 seconds ago' +%Y%m%d%H%M.%S)" "$work/slots/slot-1"
HEADWORK_HEAVY_SLOTS=1 scripts/heavy.sh --no-wait true 2> "$work/old.err" && old=0 || old=$?
check "an old slot with an unwritten owner is taken over" "[ '$old' -eq 0 ]"

unset HEADWORK_HEAVY_DIR
shared=$(bash -c '. scripts/heavy.sh; heavy_dir')
shared_ok=0
case "$shared" in
    */heavy-slots | */.heavy-slots) shared_ok=1 ;;
    *) ;;
esac
check "the default slots folder is the machine-wide one" "[ '$shared_ok' -eq 1 ]"

if [ "$failures" -gt 0 ]; then
    echo "$failures heavy.sh tests failed"
    exit 1
fi
echo "all heavy.sh tests passed"
