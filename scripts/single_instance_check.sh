#!/usr/bin/env bash
#
# The single-instance audit (§2 rule 1), against two real launches of the built
# binary — not against the lock library's unit tests.
#
#   scripts/single_instance_check.sh              build (debug) and check
#   scripts/single_instance_check.sh --no-build   check the binary already built
#
# What it proves, with a throwaway $HOME so nothing touches the real ~/.evo:
#
#   1. the first launch takes the lock and opens its window;
#   2. a second launch knocks, exits 0 within about a second, and the first one
#      comes forward (logged as "activated by another launch");
#   3. killing the first one leaves no stale lock: the next launch becomes the
#      primary.
#
# The build goes through cargo, so it takes the shared target dir's lock and
# waits for any other gpui build rather than racing it.
set -euo pipefail

case "${1:-}" in
--no-build) build=no ;;
"") build=yes ;;
*)
    echo "usage: ${0##*/} [--no-build]" >&2
    exit 2
    ;;
esac

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
BIN="${EVO_DESKTOP_BIN:-$TARGET/debug/evo-desktop}"

# The app is a GUI program: it must stay up while we knock on it.
PRIMARY_START_TIMEOUT=${PRIMARY_START_TIMEOUT:-30}
SECOND_LAUNCH_LIMIT_SECS=${SECOND_LAUNCH_LIMIT_SECS:-3}
ACTIVATION_TIMEOUT=${ACTIVATION_TIMEOUT:-15}

failures=0
note() { printf '%s\n' "$*"; }
ok() { printf '  ok   %s\n' "$*"; }
bad() {
    printf '  FAIL %s\n' "$*" >&2
    failures=$((failures + 1))
}

cleanup() {
    for pid in "${PRIMARY:-}" "${THIRD:-}"; do
        if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
            kill -TERM "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        fi
    done
    if [ -n "${HOME_DIR:-}" ]; then
        # A killed app leaves its catalog probes to notice for themselves —
        # `EVO_SERVE_WATCH_PID` is what tells them, within a couple of seconds.
        # This is only the belt to that pair of braces.
        pkill -f "token-file $HOME_DIR/" 2>/dev/null || true
        rm -rf "$HOME_DIR"
    fi
}
trap cleanup EXIT

if [ "$build" = yes ]; then
    note "building evo-desktop (debug, $TARGET)…"
    CARGO_TARGET_DIR="$TARGET" cargo build --manifest-path "$ROOT/Cargo.toml" -p evo-desktop
fi
[ -x "$BIN" ] || {
    echo "${0##*/}: no binary at $BIN" >&2
    exit 2
}

HOME_DIR="$(mktemp -d "${TMPDIR:-/tmp}/evo-desktop-si-XXXXXX")"
STATE="$HOME_DIR/.evo/desktop"
LOCK="$STATE/lock"
LOG="$STATE/app.log"

wait_for() { # wait_for <seconds> <command…>
    local deadline=$(( $(date -u +%s) + $1 ))
    shift
    while [ "$(date -u +%s)" -lt "$deadline" ]; do
        if "$@" >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.2
    done
    return 1
}

lock_pid() { sed -n '1p' "$LOCK" 2>/dev/null | tr -d '[:space:]'; }
log_lines() { wc -l <"$LOG" 2>/dev/null | tr -d '[:space:]'; }
has_line() { grep -qF "$1" "$LOG" 2>/dev/null; }

# --- 1. the first launch is the primary ------------------------------------
note "1. first launch takes the lock"
HOME="$HOME_DIR" "$BIN" >"$HOME_DIR/primary.out" 2>&1 &
PRIMARY=$!
# Out of the job table: the shell would otherwise print "Terminated" when we
# kill it below, which reads like a failure.
disown "$PRIMARY" 2>/dev/null || true
if wait_for "$PRIMARY_START_TIMEOUT" test -s "$LOCK"; then
    ok "lock written by pid $(lock_pid)"
else
    bad "no lock after ${PRIMARY_START_TIMEOUT}s (see $HOME_DIR/primary.out)"
    exit 1
fi
if wait_for "$PRIMARY_START_TIMEOUT" has_line "window open"; then
    ok "the window opened"
else
    bad "the window never opened (see $HOME_DIR/primary.out)"
fi
if [ "$(lock_pid)" = "$PRIMARY" ]; then
    ok "the lock names the process we started"
else
    bad "lock pid $(lock_pid) is not the launched pid $PRIMARY"
fi

# --- 2. a second launch knocks and leaves ----------------------------------
note "2. second launch asks the first to come forward"
before=$(log_lines)
started=$(date -u +%s)
set +e
HOME="$HOME_DIR" "$BIN" >"$HOME_DIR/second.out" 2>&1
second_status=$?
set -e
took=$(( $(date -u +%s) - started ))
if [ "$second_status" -eq 0 ]; then
    ok "the second launch exited 0"
else
    bad "the second launch exited $second_status"
fi
if [ "$took" -le "$SECOND_LAUNCH_LIMIT_SECS" ]; then
    ok "…within ${took}s"
else
    bad "…but took ${took}s (limit ${SECOND_LAUNCH_LIMIT_SECS}s)"
fi
if grep -qF "another instance is running" "$HOME_DIR/second.out"; then
    ok "it says why (another instance is running)"
else
    bad "the second launch did not report finding another instance"
fi
if wait_for "$ACTIVATION_TIMEOUT" has_line "activated by another launch"; then
    ok "the primary was activated by the knock"
else
    bad "the primary never logged an activation"
fi
if [ "$(lock_pid)" = "$PRIMARY" ] && kill -0 "$PRIMARY" 2>/dev/null; then
    ok "the second launch did not disturb the primary"
else
    bad "the primary is gone or the lock changed"
fi
[ "$(log_lines)" -gt "$before" ] || bad "the primary logged nothing new"

# --- 3. killing the primary leaves no stale lock ---------------------------
note "3. a killed primary leaves no stale lock"
kill -TERM "$PRIMARY"
if wait_for 10 sh -c "! kill -0 $PRIMARY 2>/dev/null"; then
    ok "the primary is gone"
else
    bad "the primary would not stop"
fi
wait "$PRIMARY" 2>/dev/null || true
PRIMARY=""

before=$(log_lines)
HOME="$HOME_DIR" "$BIN" >"$HOME_DIR/third.out" 2>&1 &
THIRD=$!
disown "$THIRD" 2>/dev/null || true
if wait_for "$PRIMARY_START_TIMEOUT" test -s "$LOCK"; then
    if [ "$(lock_pid)" = "$THIRD" ]; then
        ok "the next launch became the primary"
    else
        bad "the lock names $(lock_pid), not the new launch $THIRD"
    fi
else
    bad "no lock after the primary was killed"
fi
if tail -n "+$((before + 1))" "$LOG" 2>/dev/null | grep -qF "another instance is running"; then
    bad "the new launch thought another instance was running"
else
    ok "the new launch found no other instance"
fi

kill -TERM "$THIRD" 2>/dev/null || true
wait "$THIRD" 2>/dev/null || true
THIRD=""

note ""
if [ "$failures" -eq 0 ]; then
    note "single instance: all checks passed"
else
    note "single instance: $failures check(s) failed"
    exit 1
fi
