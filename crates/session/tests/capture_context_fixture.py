#!/usr/bin/env python3
"""capture_context_fixture.py — capture one injected-context /transcript.

An extension injects content into a session with `evo:inject-context`, which
journals a `:custom-message` entry; the fold tags that message `:meta (:key
...)`.  The memory extension does this at the start of every fresh session, so
this boots the same stub-provider swarm the other fixtures come from, with a
memory store in the temp HOME and one in the temp project, asks one question,
and records `/transcript` whole.

  python3 crates/session/tests/capture_context_fixture.py

The fixture is what proves the wire shape the session model reads: a user-role
message carrying `meta: {key: <key>}`.

Log lines are UTC.
"""

import http.client
import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone

EVO_AGENT = "/Users/bytedance/coding/evo-agent"
SWARM = "/usr/local/bin/evo-swarm"
EVO = "/usr/local/bin/evo-agent"
HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "fixtures")
SECRET = "fixtures-stub-provider-secret"

GLOBAL_MEMORY = [
    ("mem-ctx-global-1", "constraint",
     "Fixture memory: the coffee machine on floor 3 is broken."),
    ("mem-ctx-global-2", "convention",
     "Fixture memory: commit messages name the crate in parentheses."),
]
PROJECT_MEMORY = [
    ("mem-ctx-project-1", "fact",
     "Fixture memory: the fixture project lives in a temp directory."),
]


def log(message):
    stamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    print(f"{stamp} {message}", flush=True)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def memory_line(entry_id, kind, text):
    """One entry as `write-sexpr-line` writes it: a plist on one line."""
    stamp = "2026-09-30T00:00:00Z"
    return (f'(:id "{entry_id}" :kind :{kind} :text "{text}" '
            f':created-at "{stamp}" :updated-at "{stamp}")\n')


def request(port, token, method, path, body=None, timeout=60):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    data = json.dumps(body).encode() if body is not None else None
    headers = {"Authorization": f"Bearer {token}"}
    if data:
        headers["Content-Type"] = "application/json"
    conn.request(method, path, body=data, headers=headers)
    resp = conn.getresponse()
    raw = resp.read().decode(errors="replace")
    conn.close()
    try:
        return resp.status, json.loads(raw)
    except ValueError:
        return resp.status, raw


def wait_for(predicate, timeout=120, interval=0.2):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            value = predicate()
        except Exception:                                        # noqa: BLE001
            value = None
        if value:
            return value
        time.sleep(interval)
    return None


def main():
    os.makedirs(OUT, exist_ok=True)
    work = tempfile.mkdtemp(prefix="evo-context-fixture-")
    home = os.path.join(work, "home")
    proj = os.path.join(work, "proj")
    os.makedirs(home)
    os.makedirs(os.path.join(proj, ".evo"))
    log(f"work {work}")

    stub_port = free_port()
    stub = subprocess.Popen([sys.executable,
                             os.path.join(EVO_AGENT, "tests", "stub-messages.py"),
                             str(stub_port)],
                            stdout=subprocess.PIPE, text=True)
    line = stub.stdout.readline()
    assert line.startswith("stub listening"), line
    log(f"stub provider on port {stub_port}")

    with open(os.path.join(home, "init.lisp"), "w") as f:
        f.write(f'(evo:register-provider :stub :base-url "http://127.0.0.1:{stub_port}" '
                f':api-key "{SECRET}")\n'
                '(evo:register-model "stub-a" :provider :stub :context-window 200000 '
                ':max-output 8000 :effort t)\n'
                '(evo:set-setting :model "stub-a")\n')

    # The stores the memory extension snapshots into the session at its start.
    with open(os.path.join(home, "memory.sexp"), "w") as f:
        for entry in GLOBAL_MEMORY:
            f.write(memory_line(*entry))
    with open(os.path.join(proj, ".evo", "memory.sexp"), "w") as f:
        for entry in PROJECT_MEMORY:
            f.write(memory_line(*entry))

    port = free_port()
    token_file = os.path.join(work, "token")
    env = dict(os.environ, EVO_HOME=home, EVO_BINARY=EVO, TERM="xterm-256color")
    for var in ("EVO_SERVE_TOKEN", "EVO_SUPERVISED_CHILD", "EVO_NO_SUPERVISOR",
                "EVO_SESSIONS_DIR", "EVO_SERVE_WATCH_PID", "ANTHROPIC_API_KEY"):
        env.pop(var, None)
    log_file = open(os.path.join(work, "swarm.log"), "w")
    swarm = subprocess.Popen([SWARM, "serve", "--port", str(port),
                              "--token-file", token_file, "--workers", "1",
                              "--evo", EVO],
                             cwd=proj, env=env, stdin=subprocess.DEVNULL,
                             stdout=log_file, stderr=subprocess.STDOUT,
                             start_new_session=True)
    try:
        token = wait_for(lambda: open(token_file).read().strip()
                         if os.path.exists(token_file) and os.path.getsize(token_file) else None,
                         timeout=90)
        if not token:
            raise RuntimeError("the swarm never wrote its token")
        if not wait_for(lambda: request(port, token, "GET", "/health")[0] == 200, timeout=90):
            raise RuntimeError("the swarm never became ready")
        log("swarm up")

        # One prompt: a fresh session is what injects the snapshots, and its
        # transcript is what carries them.
        status, reply = request(port, token, "POST", "/prompt", {"text": "Say hello."})
        log(f"/prompt -> {status}")
        if status != 200:
            raise RuntimeError(f"/prompt failed: {reply}")
        if not wait_for(lambda: request(port, token, "GET", "/state")[1]["status"] == "idle",
                        timeout=120):
            raise RuntimeError("the run never went idle")

        status, transcript = request(port, token, "GET", "/transcript")
        if status != 200:
            raise RuntimeError(f"/transcript -> {status}")
        path = os.path.join(OUT, "context-transcript.json")
        with open(path, "w") as f:
            json.dump(transcript, f, indent=2, ensure_ascii=False)
            f.write("\n")
        log(f"wrote context-transcript.json ({os.path.getsize(path)} bytes)")

        for message in transcript["messages"]:
            log(f"  message role={message.get('role')!r} meta={message.get('meta')!r} "
                f"chars={len(json.dumps(message.get('content')))}")

        request(port, token, "POST", "/shutdown")
        wait_for(lambda: swarm.poll() is not None, timeout=30)
    finally:
        if swarm.poll() is None:
            try:
                os.killpg(swarm.pid, signal.SIGKILL)
            except OSError:
                swarm.kill()
        log_file.close()
        stub.kill()


if __name__ == "__main__":
    main()
