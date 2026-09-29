#!/usr/bin/env bash
#
# The gate: "is this tree green?" — formatting, lints, and every crate's tests.
#
#   scripts/check.sh          check the working tree
#   scripts/check.sh --head   check HEAD in a worktree of its own
#
# Three steps, in order, and a table at the end:
#
#   1. `cargo fmt -p <crate> -- --check` — every crate, one at a time, no files
#      rewritten. Per crate on purpose: a lane owns its crates' formatting, and a
#      single `--all` line would say only "the tree is red" while someone else's
#      files were the ones rustfmt wanted.
#   2. `cargo clippy --workspace --all-targets` — warnings are listed and counted
#   3. `cargo test -p <crate>`          — every crate, one at a time
#
# The tests go through the per-crate target directories the lanes use
# (`target/session`, `target/tab_engine`, `target/proofs`, `target/app`, …): each
# lane builds and tests on its own, so sharing one directory would put every run
# behind the others' locks — and behind their in-flight edits, which is what
# `--head` is for.
#
# `--head` is the honest mode for the question "is what we committed green?": it
# adds a detached worktree of `HEAD` under `$TMPDIR/evo-desktop-check-head`, runs
# the same three steps through the very same script, and removes the worktree
# again — so the answer cannot be coloured by anything uncommitted. The target
# directory is `target/head` (in this checkout), a *fixed* path: the big builds
# behind these crates — gpui above all — are cached there and reused by the next
# run. The worktree itself is fresh each time, so the workspace's own crates are
# rebuilt, which is the honest half of that trade.
#
# Nothing here is destructive: no build products are removed, no files are
# rewritten (`fmt --check` only reads), and the worktree is one git makes and
# takes back. Logs of every step land in `target/check/`, kept after the run —
# under `--head` they are the only copy, since the tree they came from is gone.
#
# Run it on an idle machine. These crates start real servers and real swarms, and a
# second `cargo` (another lane's, another target dir) on the same box shows up as
# timeouts in the swarm tests rather than as anything to do with this tree.
set -euo pipefail

usage() {
    sed -n '3,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

head_mode=no
case "${1:-}" in
--head) head_mode=yes ;;
-h | --help)
    usage
    exit 0
    ;;
"")
    ;;
*)
    usage >&2
    exit 2
    ;;
esac

# Every crate in the workspace, in the order the gate runs them: the pure-Rust
# ones first (seconds), then the proof crate (four real swarms, ~50s), then the
# two that build gpui.
CRATES=(swarm_client tab_engine store session transcript composer agent_list proofs workspace evo-desktop)

# The target directory a crate's tests use. `crates/app` is the `evo-desktop`
# package and builds in `target/app`, as the lanes do.
target_dir_of() {
    case "$1" in
    evo-desktop) printf 'app\n' ;;
    *) printf '%s\n' "$1" ;;
    esac
}

ORIG_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="$ORIG_ROOT"
LOG_DIR="$ORIG_ROOT/target/check"
mkdir -p "$LOG_DIR"

commit="$(git -C "$ORIG_ROOT" rev-parse --short HEAD)"
dirty=""
if [ -n "$(git -C "$ORIG_ROOT" status --porcelain)" ]; then
    dirty=" (working tree has uncommitted changes)"
fi

failures=0
note() { printf '%s\n' "$*"; }
ok() { printf '  ok   %s\n' "$*"; }
bad() {
    printf '  FAIL %s\n' "$*" >&2
    failures=$((failures + 1))
}

# --- the tree under test ----------------------------------------------------
if [ "$head_mode" = yes ]; then
    TARGET_BASE="$ORIG_ROOT/target/head"
    HEAD_DIR="${TMPDIR:-/tmp}/evo-desktop-check-head"
    remove_head() {
        git -C "$ORIG_ROOT" worktree remove --force "$HEAD_DIR" >/dev/null 2>&1 || true
        git -C "$ORIG_ROOT" worktree prune >/dev/null 2>&1 || true
    }
    trap remove_head EXIT
    remove_head
    git -C "$ORIG_ROOT" worktree prune
    rm -rf "$HEAD_DIR"
    git -C "$ORIG_ROOT" worktree add --detach "$HEAD_DIR" HEAD >/dev/null
    ROOT="$HEAD_DIR"
    PREFIX="head-"
    note "check: HEAD $commit$dirty, in a worktree at $HEAD_DIR"
else
    TARGET_BASE="$ORIG_ROOT/target"
    PREFIX=""
    note "check: the working tree at $ROOT (HEAD $commit$dirty)"
fi
note "logs: $LOG_DIR"
note ""

# `test result: ok. 12 passed; 0 failed; …` lines, summed over every binary the
# crate runs (unit tests, integration tests, doc tests).
sum_results() {
    awk '/^test result:/ {
             for (i = 1; i <= NF; i++) {
                 if ($i == "passed;") p += $(i - 1)
                 if ($i == "failed;") f += $(i - 1)
             }
         }
         END { printf "%d %d", p + 0, f + 0 }' "$1"
}

# --- 1. each crate's formatting ---------------------------------------------
# Checked per crate, in the loop below with its tests: `cargo fmt -p <crate>` covers
# that package's own targets — lib, bins, tests, examples — which is what a lane is
# answerable for. The whole-tree line at the end says whether the workspace as a
# whole is clean.

# --- 2. lints ---------------------------------------------------------------
note "== clippy (cargo clippy --workspace --all-targets) =="
# The workspace's own build directory: it is where the UI crates already are, so
# this does not build gpui a second time.
clippy_log="$LOG_DIR/${PREFIX}clippy.log"
started=$(date -u +%s)
if (cd "$ROOT" && CARGO_TARGET_DIR="$ORIG_ROOT/target" cargo clippy --workspace --all-targets) >"$clippy_log" 2>&1; then
    clippy_errors=0
else
    clippy_errors=1
fi
clippy_secs=$(( $(date -u +%s) - started ))
clippy_warnings=$(grep -c '^warning' "$clippy_log" || true)
clippy_warnings=$(( clippy_warnings - $(grep -c 'generated .* warning' "$clippy_log" || true) ))
if [ "$clippy_errors" -eq 0 ] && [ "$clippy_warnings" -eq 0 ]; then
    clippy_status=ok
    ok "no warnings (${clippy_secs}s)"
elif [ "$clippy_errors" -eq 0 ]; then
    clippy_status="warn"
    bad "$clippy_warnings warning(s), no errors (${clippy_secs}s) — see $clippy_log"
    grep -E '^(warning|error)' "$clippy_log" | grep -v 'generated ' | sort | uniq -c | sort -rn | head -15 | sed 's/^/       /'
    grep -E '^ +--> ' "$clippy_log" | head -15 | sed 's/^/       /'
else
    clippy_status=FAIL
    bad "clippy failed (${clippy_secs}s) — see $clippy_log"
    grep -E '^(warning|error)' "$clippy_log" | grep -v 'generated ' | sort | uniq -c | sort -rn | head -15 | sed 's/^/       /'
    grep -E '^ +--> ' "$clippy_log" | head -15 | sed 's/^/       /'
fi

# --- 3. every crate's tests -------------------------------------------------
note "== tests =="
declare -a rows=()
for crate in "${CRATES[@]}"; do
    fmt_log="$LOG_DIR/${PREFIX}${crate}.fmt.log"
    if (cd "$ROOT" && cargo fmt -p "$crate" -- --check) >"$fmt_log" 2>&1; then
        crate_fmt=ok
    else
        crate_hunks=$(grep -c '^Diff in ' "$fmt_log" || true)
        crate_fmt="$(grep -o '^Diff in [^:]*' "$fmt_log" | sort -u | wc -l | tr -d ' ') files"
        failures=$((failures + 1))
        bad "$crate: rustfmt would rewrite $crate_hunks hunk(s) in $crate_fmt — see $fmt_log"
    fi

    log="$LOG_DIR/${PREFIX}${crate}.test.log"
    dir="$TARGET_BASE/$(target_dir_of "$crate")"
    started=$(date -u +%s)
    if (cd "$ROOT" && CARGO_TARGET_DIR="$dir" cargo test -p "$crate") >"$log" 2>&1; then
        status=ok
    else
        status=FAIL
    fi
    secs=$(( $(date -u +%s) - started ))
    read -r passed failed <<<"$(sum_results "$log")"
    rows+=("| $crate | $crate_fmt | $passed | $failed | ${secs}s | $status |")
    if [ "$status" = ok ] && [ "$crate_fmt" = ok ]; then
        ok "$crate: rustfmt-clean, $passed passed, $failed failed (${secs}s)"
    elif [ "$status" = ok ]; then
        ok "$crate: tests $passed passed, $failed failed (${secs}s)"
    else
        bad "$crate: $passed passed, $failed failed (${secs}s) — see $log"
        failures=$((failures + 1))
        grep -E '^(error|---- .* stdout|test .* FAILED|failures:)' "$log" | head -8 | sed 's/^/       /'
    fi
done

# --- the table --------------------------------------------------------------
note ""
note "| step | seconds | status |"
note "|---|---:|---|"
note "| cargo clippy --workspace --all-targets | ${clippy_secs}s | $clippy_status ($clippy_warnings warnings) |"
note ""
note "| crate | fmt | passed | failed | seconds | tests |"
note "|---|---|---:|---:|---:|---|"
for row in "${rows[@]}"; do
    note "$row"
done

# The whole tree, for the record: every file rustfmt would rewrite, across the
# crates — the per-crate column above is the one a lane acts on.
fmt_log="$LOG_DIR/${PREFIX}fmt.log"
if (cd "$ROOT" && cargo fmt --all -- --check) >"$fmt_log" 2>&1; then
    note ""
    note "the whole workspace is rustfmt-clean"
else
    fmt_files=$(grep -o '^Diff in [^:]*' "$fmt_log" | sort -u | wc -l | tr -d ' ')
    note ""
    note "the whole workspace: rustfmt would rewrite $fmt_files file(s) — see $fmt_log"
fi

note ""
if [ "$failures" -eq 0 ]; then
    note "check: green (HEAD $commit$dirty, $(date -u +%Y-%m-%dT%H:%M:%SZ))"
else
    note "check: $failures step(s) failed (HEAD $commit$dirty, $(date -u +%Y-%m-%dT%H:%M:%SZ))"
    exit 1
fi
