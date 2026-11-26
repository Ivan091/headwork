#!/usr/bin/env bash

heavy_dir() {
    if [ -n "${HEADWORK_HEAVY_DIR:-}" ]; then
        printf '%s' "$HEADWORK_HEAVY_DIR"
        return
    fi
    if [ -n "${LOCALAPPDATA:-}" ]; then
        local base=$LOCALAPPDATA
        if command -v cygpath >/dev/null 2>&1; then
            base=$(cygpath -u "$LOCALAPPDATA")
        fi
        printf '%s/heavy-slots' "$base"
        return
    fi
    printf '%s/.heavy-slots' "$HOME"
}

heavy_slots() {
    local slots=${HEADWORK_HEAVY_SLOTS:-3}
    case "$slots" in
        '' | *[!0-9]* | 0) slots=3 ;;
        *) ;;
    esac
    printf '%s' "$slots"
}

heavy_ungated() {
    [ -n "${CI:-}" ] || [ "${HEADWORK_HEAVY_OFF:-0}" = 1 ]
}

heavy_field() {
    sed -n "s/^$2=//p" "$1/owner" 2>/dev/null | head -n 1
}

heavy_age() {
    local now made
    now=$(date +%s)
    made=$(stat -c %Y "$1" 2>/dev/null || stat -f %m "$1" 2>/dev/null || echo "$now")
    echo $((now - made))
}

heavy_stale() {
    local slot=$1 pid
    [ -d "$slot" ] || return 1
    pid=$(heavy_field "$slot" pid)
    if [ -n "$pid" ]; then
        ! kill -0 "$pid" 2>/dev/null
    else
        [ "$(heavy_age "$slot")" -gt 10 ]
    fi
}

heavy_reap() {
    local slot=$1 lock
    lock="$(dirname "$slot")/reap.lock"
    if [ -d "$lock" ] && [ "$(heavy_age "$lock")" -gt 30 ]; then
        rmdir "$lock" 2>/dev/null || true
    fi
    mkdir "$lock" 2>/dev/null || return 0
    if heavy_stale "$slot"; then
        echo "heavy: taking over $(basename "$slot"), whose holder is gone" >&2
        rm -rf "$slot"
    fi
    rmdir "$lock"
}

heavy_try() {
    local dir=$1 label=$2 i slot top
    top=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
    for i in $(seq 1 "$(heavy_slots)"); do
        slot="$dir/slot-$i"
        heavy_stale "$slot" && heavy_reap "$slot"
        if mkdir "$slot" 2>/dev/null; then
            printf 'pid=%s\nworktree=%s\ncommand=%s\nstarted=%s\n' "$$" "$top" "$label" "$(date '+%Y-%m-%d %H:%M:%S')" \
                > "$slot/owner"
            HEAVY_SLOT=$slot
            return 0
        fi
    done
    return 1
}

heavy_holders() {
    local slot
    for slot in "$1"/slot-*; do
        [ -d "$slot" ] || continue
        printf '  %s: pid %s, %s, since %s: %s\n' "$(basename "$slot")" "$(heavy_field "$slot" pid)" \
            "$(heavy_field "$slot" worktree)" "$(heavy_field "$slot" started)" "$(heavy_field "$slot" command)"
    done
}

heavy_acquire() {
    local no_wait=${HEADWORK_HEAVY_NO_WAIT:-0} label dir every waited=0
    if [ "${1:-}" = --no-wait ]; then
        no_wait=1
        shift
    fi
    label=${1:-$0}
    dir=$(heavy_dir)
    if ! mkdir -p "$dir" 2>/dev/null; then
        echo "heavy: cannot create $dir; running without a slot" >&2
        return 0
    fi
    every=${HEADWORK_HEAVY_REPORT:-30}
    [ "$every" -gt 0 ] 2>/dev/null || every=30
    while ! heavy_try "$dir" "$label"; do
        if [ "$no_wait" = 1 ]; then
            echo "heavy: no free slot of $(heavy_slots) for $label; held by" >&2
            heavy_holders "$dir" >&2
            return 75
        fi
        if [ $((waited % every)) -eq 0 ]; then
            echo "heavy: waiting $waited s for one of $(heavy_slots) slots for $label; held by" >&2
            heavy_holders "$dir" >&2
        fi
        sleep 1
        waited=$((waited + 1))
    done
    export HEADWORK_HEAVY_HELD=1
}

heavy_release() {
    if [ -n "${HEAVY_SLOT:-}" ]; then
        rm -rf "$HEAVY_SLOT"
        HEAVY_SLOT=""
        unset HEADWORK_HEAVY_HELD
    fi
}

heavy_hold() {
    if heavy_ungated || [ "${HEADWORK_HEAVY_HELD:-0}" = 1 ] || [ -n "${HEAVY_SLOT:-}" ]; then
        return 0
    fi
    heavy_acquire "$@" || return
    trap 'heavy_release' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
}

heavy() {
    if heavy_ungated || [ "${HEADWORK_HEAVY_HELD:-0}" = 1 ]; then
        "$@"
        return
    fi
    local no_wait=()
    if [ "${1:-}" = --no-wait ]; then
        no_wait=(--no-wait)
        shift
    fi
    heavy_acquire ${no_wait[@]+"${no_wait[@]}"} "$*" || return
    local status=0
    "$@" || status=$?
    heavy_release
    return "$status"
}

heavy_status() {
    local dir slot state
    dir=$(heavy_dir)
    echo "heavy slots: $(heavy_slots) in $dir"
    for slot in "$dir"/slot-*; do
        [ -d "$slot" ] || continue
        state=running
        heavy_stale "$slot" && state=stale
        printf '  %s (%s): pid %s, %s, since %s: %s\n' "$(basename "$slot")" "$state" "$(heavy_field "$slot" pid)" \
            "$(heavy_field "$slot" worktree)" "$(heavy_field "$slot" started)" "$(heavy_field "$slot" command)"
    done
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    set -u
    if [ "${1:-}" = status ]; then
        heavy_status
        exit
    fi
    if [ $# -eq 0 ] || { [ "$1" = --no-wait ] && [ $# -lt 2 ]; }; then
        echo "usage: scripts/heavy.sh [--no-wait] <cmd...> | status" >&2
        exit 2
    fi
    trap 'heavy_release' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    heavy "$@"
fi
