#!/usr/bin/env bash
#
# A throwaway HOME whose evo home registers the scripted stub provider, so
# evo-desktop — or evo-swarm, or evo-agent — can be run against a free model:
# no API key, no network, and nothing written to the real ~/.evo.
#
#   scripts/stub_home.sh run [--keep] -- <command> [args...]
#       A fresh temp HOME, the stub up, then the command with HOME and EVO_HOME
#       set. The stub is stopped when the command ends and the HOME is removed
#       if the command succeeded — kept when it failed, or with --keep /
#       STUB_HOME_KEEP=1, so its journals and logs are there to read.
#
#   scripts/stub_home.sh start [DIR]
#       Start a stub for a long session and print how to use it.
#
#   scripts/stub_home.sh exports <DIR>   # the exports for an existing HOME
#   scripts/stub_home.sh stop <DIR>      # stop its stub; the HOME is kept
#
# The model is ../evo-agent/tests/stub-messages.py (EVO_STUB_MESSAGES or
# EVO_AGENT_REPO override where it is found) — the same scripted model
# ../evo-agent/tests/swarm-serve-e2e.py and crates/swarm_client/src/harness.rs
# drive. Read its docstring for what it answers: "CALL <tool> {json}", "SLOW "
# for a six-second stream, "DELAY2 " to wait, anything else echoes.
#
# The command also gets STUB_URL, so `GET $STUB_URL/_requests` (or a prompt
# like "CALL bash {…}") shows what evo really sent.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The model init.lisp registers, and the dummy key it is registered with. The
# stub never checks it; it exists because a provider needs one.
STUB_MODEL="stub-a"
STUB_SECRET="evo-desktop-stub-secret"

# What a stub home must not inherit from the environment it was started in.
# The list is ../evo-agent/tests/swarm-serve-e2e.py's, plus the variables this
# machine's own agents export, plus the real provider keys.
UNSET_VARS=(
    EVO_HOME EVO_SESSIONS_DIR EVO_SERVE_TOKEN EVO_SERVE_WATCH_PID
    EVO_SUPERVISED_CHILD EVO_NO_SUPERVISOR EVO_PID EVO_HEARTBEAT_FILE
    EVO_IDE_CONTEXT ANTHROPIC_API_KEY OPENAI_API_KEY
)

# $TMPDIR ends in a slash on macOS; the printed paths should not double it.
TMP="${TMPDIR:-/tmp}"
TMP="${TMP%/}"

STUB_PID=""
STUB_PORT=""
CURRENT_HOME=""
KEEP_HOME=""

usage() {
    # The header comment, without its `#`, up to the first line of code.
    awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "${BASH_SOURCE[0]}"
}

die() {
    echo "stub_home: $*" >&2
    exit 1
}

# Where the scripted model lives.
stub_script() {
    if [ -n "${EVO_STUB_MESSAGES:-}" ]; then
        printf '%s\n' "$EVO_STUB_MESSAGES"
        return 0
    fi
    local candidates=()
    if [ -n "${EVO_AGENT_REPO:-}" ]; then
        candidates+=("$EVO_AGENT_REPO/tests/stub-messages.py")
    fi
    candidates+=("$ROOT/../evo-agent/tests/stub-messages.py")
    local candidate
    for candidate in "${candidates[@]}"; do
        if [ -f "$candidate" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    echo "stub_home: no stub-messages.py; set EVO_STUB_MESSAGES or EVO_AGENT_REPO (tried: ${candidates[*]})" >&2
    return 1
}

# start_home HOME — start the stub and write HOME/.evo/init.lisp.
# Sets STUB_PID and STUB_PORT.
start_home() {
    local home="$1"
    local stub
    stub="$(stub_script)" || return 1
    mkdir -p "$home/.evo"

    # Port 0: the stub picks a free port itself and prints it on the first line
    # of its log, so nothing races for the number here.
    : >"$home/stub.log"
    python3 "$stub" 0 >>"$home/stub.log" 2>&1 &
    local pid=$!
    local port=""
    local deadline=$((SECONDS + 10))
    while [ -z "$port" ]; do
        if ! kill -0 "$pid" 2>/dev/null; then
            echo "stub_home: the stub model exited at once:" >&2
            sed 's/^/  /' "$home/stub.log" >&2
            return 1
        fi
        port="$(sed -n 's/^stub listening \([0-9][0-9]*\).*/\1/p' "$home/stub.log" | head -n 1)"
        if [ -z "$port" ]; then
            if [ "$SECONDS" -ge "$deadline" ]; then
                echo "stub_home: the stub model did not start within 10s" >&2
                return 1
            fi
            sleep 0.05
        fi
    done

    cat >"$home/.evo/init.lisp" <<LISP
(evo:register-provider :stub :base-url "http://127.0.0.1:$port" :api-key "$STUB_SECRET")
(evo:register-model "$STUB_MODEL" :provider :stub :context-window 200000 :max-output 8000 :effort t)
(evo:set-setting :model "$STUB_MODEL")
LISP
    cat >"$home/.stub-home" <<META
pid=$pid
port=$port
started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
stub=$stub
META
    STUB_PID="$pid"
    STUB_PORT="$port"
    return 0
}

# stop_home HOME — stop the stub recorded in that HOME. Keeps the directory.
stop_home() {
    local home="$1"
    local meta="$home/.stub-home"
    if [ ! -f "$meta" ]; then
        echo "stub_home: no stub recorded in $home" >&2
        return 0
    fi
    local pid
    pid="$(sed -n 's/^pid=//p' "$meta" | head -n 1)"
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
        kill "$pid" 2>/dev/null || true
        local i
        for i in $(seq 1 50); do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.1
        done
        if kill -0 "$pid" 2>/dev/null; then
            kill -9 "$pid" 2>/dev/null || true
        fi
    fi
    rm -f "$meta"
    return 0
}

# run_env HOME COMMAND... — the command, with the stub home's environment.
run_env() {
    local home="$1"
    shift
    local -a unset=() extra=()
    local var
    for var in "${UNSET_VARS[@]}"; do
        unset+=(-u "$var")
    done
    # Point the swarm's lanes at the installed agent unless the caller has an
    # opinion; it is what `--evo` would otherwise have to say.
    if [ -n "${EVO_BINARY:-}" ]; then
        extra+=(EVO_BINARY="$EVO_BINARY")
    elif [ -x /usr/local/bin/evo-agent ]; then
        extra+=(EVO_BINARY=/usr/local/bin/evo-agent)
    fi
    env "${unset[@]}" "${extra[@]}" \
        HOME="$home" EVO_HOME="$home/.evo" TERM="${TERM:-xterm-256color}" \
        STUB_URL="http://127.0.0.1:${STUB_PORT:-$(stub_port "$home")}" \
        "$@"
}

stub_port() {
    sed -n 's/^port=//p' "$1/.stub-home" 2>/dev/null | head -n 1
}

print_exports() {
    local home="$1"
    [ -f "$home/.evo/init.lisp" ] || die "$home does not look like a stub home"
    printf 'export HOME=%q\n' "$home"
    printf 'export EVO_HOME=%q\n' "$home/.evo"
}

# The HOME a `run` is using, cleaned up however the script ends.
on_exit() {
    local status=$?
    if [ -n "$CURRENT_HOME" ]; then
        stop_home "$CURRENT_HOME" >/dev/null 2>&1 || true
        if [ "$status" -eq 0 ] && [ -z "$KEEP_HOME" ]; then
            rm -rf "$CURRENT_HOME"
        else
            echo "stub_home: kept $CURRENT_HOME (exit $status)" >&2
        fi
        CURRENT_HOME=""
    fi
    exit "$status"
}

case "${1:-}" in
run)
    shift
    while [ $# -gt 0 ]; do
        case "$1" in
        --keep) KEEP_HOME=1 ;;
        --) shift && break ;;
        *) break ;;
        esac
        shift
    done
    [ $# -gt 0 ] || die "run needs a command: ${0##*/} run -- <command> [args...]"
    if [ -n "${STUB_HOME_KEEP:-}" ]; then
        KEEP_HOME=1
    fi
    home="$(mktemp -d "$TMP/evo-stub-home.XXXXXX")"
    CURRENT_HOME="$home"
    trap on_exit EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    start_home "$home" || die "could not start the stub in $home"
    # On stderr: a command's stdout stays its own.
    echo "stub_home: HOME=$home STUB_URL=http://127.0.0.1:$STUB_PORT (log $home/stub.log)" >&2
    run_env "$home" "$@"
    ;;
start)
    home="${2:-}"
    if [ -z "$home" ]; then
        home="$(mktemp -d "$TMP/evo-stub-home.XXXXXX")"
    else
        mkdir -p "$home"
        home="$(cd "$home" && pwd)"
    fi
    start_home "$home" || die "could not start the stub in $home"
    echo "home: $home"
    echo "stub: http://127.0.0.1:$STUB_PORT (pid $STUB_PID, log $home/stub.log)"
    echo
    print_exports "$home"
    echo
    echo "stop it with: ${0##*/} stop $home    (the HOME itself stays; rm -rf it when done)"
    ;;
stop)
    [ -n "${2:-}" ] || die "stop needs the HOME: ${0##*/} stop <dir>"
    stop_home "$2"
    echo "stub_home: stopped the stub in $2"
    ;;
exports)
    [ -n "${2:-}" ] || die "exports needs the HOME: ${0##*/} exports <dir>"
    print_exports "$2"
    ;;
--help | -h | help | "")
    usage
    ;;
*)
    usage >&2
    exit 2
    ;;
esac
